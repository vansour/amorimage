//! 清理某个存储档案上「数据库里已没有记录指向」的孤儿文件。
//!
//! 和迁移一样跑在后台：扫描阶段只统计并列清单，删除阶段才真正动手，两阶段
//! 都有进度与取消。这样几千个文件也不会把单个 HTTP 请求拖到超时。
//!
//! 判定规则：文件名的 hash 若仍被本档案的记录引用就算有主、保留。迁移走掉的
//! 记录 backend 已经改到别处，留在原档案上的就是副本，可以清掉。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::db;
use crate::error::AppError;
use crate::routes::parse_name;
use crate::state::AppState;
use crate::store;

/// 预览里最多列出多少条文件名，避免进度响应体过大
const MAX_LISTED: usize = 200;
/// 进度里最多保留多少条失败原因
const MAX_ERRORS: usize = 20;

#[derive(Default)]
pub struct CleanupState {
    inner: Mutex<Option<Run>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Scan,
    Delete,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Scan => "scan",
            Phase::Delete => "delete",
        }
    }
}

struct Run {
    profile: i64,
    phase: Phase,
    /// 扫描阶段：已扫过的文件数；删除阶段：已处理的待删文件数
    total: usize,
    done: usize,
    /// 扫描到的文件总数，删除阶段不覆盖
    scanned: usize,
    orphans: usize,
    kept: usize,
    deleted: usize,
    failed: usize,
    finished: bool,
    cancelled: bool,
    errors: Vec<String>,
    sample: Vec<String>,
    cancel: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
pub struct Progress {
    pub running: bool,
    /// 是否跑过至少一次（无论成败）
    pub present: bool,
    pub profile: i64,
    /// "scan" 或 "delete"
    pub phase: &'static str,
    pub total: usize,
    pub done: usize,
    /// 扫描到的文件总数（删除阶段也保留这个值，界面据此显示「共 N 个文件」）
    pub scanned: usize,
    pub orphans: usize,
    pub kept: usize,
    pub deleted: usize,
    pub failed: usize,
    pub cancelled: bool,
    pub errors: Vec<String>,
    pub sample: Vec<String>,
}

impl CleanupState {
    pub fn new() -> CleanupState {
        CleanupState::default()
    }

    pub fn progress(&self) -> Progress {
        match self.inner.lock().unwrap().as_ref() {
            None => Progress {
                running: false,
                present: false,
                profile: 0,
                phase: "scan",
                total: 0,
                done: 0,
                scanned: 0,
                orphans: 0,
                kept: 0,
                deleted: 0,
                failed: 0,
                cancelled: false,
                errors: Vec::new(),
                sample: Vec::new(),
            },
            Some(r) => Progress {
                running: !r.finished,
                present: true,
                profile: r.profile,
                phase: r.phase.as_str(),
                total: r.total,
                done: r.done,
                scanned: r.scanned,
                orphans: r.orphans,
                kept: r.kept,
                deleted: r.deleted,
                failed: r.failed,
                cancelled: r.cancelled,
                errors: r.errors.clone(),
                sample: r.sample.clone(),
            },
        }
    }

    pub fn is_running(&self) -> bool {
        self.inner.lock().unwrap().as_ref().is_some_and(|r| !r.finished)
    }

    fn cancel_flag(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|r| r.cancel.load(Ordering::Relaxed))
    }

    pub fn cancel(&self) -> bool {
        let guard = self.inner.lock().unwrap();
        let Some(r) = guard.as_ref().filter(|r| !r.finished) else {
            return false;
        };
        r.cancel.store(true, Ordering::Relaxed);
        true
    }
}

/// 预演：启动一次只扫描、不删除的任务
pub async fn start_scan(st: &AppState, profile: i64) -> Result<(), AppError> {
    begin(st, profile, Phase::Scan).await?;
    let st = st.clone();
    tokio::spawn(async move {
        let result = run_scan(&st, profile).await;
        finish(&st, result.err().map(|e| format!("扫描中止: {e}")));
    });
    Ok(())
}

/// 真正删除：先重新扫描一遍再删，保证清单与当前数据库状态一致
pub async fn start_delete(st: &AppState, profile: i64) -> Result<(), AppError> {
    begin(st, profile, Phase::Delete).await?;
    let st = st.clone();
    tokio::spawn(async move {
        let result = run_delete(&st, profile).await;
        finish(&st, result.err().map(|e| format!("清理中止: {e}")));
    });
    Ok(())
}

async fn begin(st: &AppState, profile: i64, phase: Phase) -> Result<(), AppError> {
    if db::storage_profile(&st.pool, profile).await?.is_none() {
        return Err(AppError::NotFound("存储档案不存在".into()));
    }
    // 迁移正在改文件与记录指向，此时清理可能把刚搬走的文件当孤儿删掉
    if st.migrate.is_running() {
        return Err(AppError::BadRequest(
            "存储迁移正在进行，请等它结束后再清理".into(),
        ));
    }
    let mut guard = st.cleanup.inner.lock().unwrap();
    if guard.as_ref().is_some_and(|r| !r.finished) {
        return Err(AppError::BadRequest("已有清理任务在进行中".into()));
    }
    *guard = Some(Run {
        profile,
        phase,
        total: 0,
        done: 0,
        scanned: 0,
        orphans: 0,
        kept: 0,
        deleted: 0,
        failed: 0,
        finished: false,
        cancelled: false,
        errors: Vec::new(),
        sample: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
    });
    Ok(())
}

fn finish(st: &AppState, error: Option<String>) {
    let mut guard = st.cleanup.inner.lock().unwrap();
    if let Some(r) = guard.as_mut() {
        if let Some(e) = error {
            r.errors.push(truncate(&e, 300));
        }
        r.cancelled = r.cancel.load(Ordering::Relaxed);
        r.finished = true;
    }
}

/// 该档案上仍算「有主」的 hash 集合。
///
/// local 档案有个坑：所有 local 档案共用同一个 AMOR_STORAGE_DIR，物理位置
/// 相同。若只按本档案的引用判定，清理其中一个会删掉另一个的文件。所以 local
/// 取所有 local 档案引用的并集。
async fn owned_scope(st: &AppState, profile: &db::ProfileRow) -> Result<Vec<i64>, AppError> {
    let mut ids = vec![profile.id];
    if profile.kind == "local" {
        for other in db::storage_profiles(&st.pool).await? {
            if other.kind == "local" && other.id != profile.id {
                ids.push(other.id);
            }
        }
    }
    Ok(ids)
}

async fn owned_hashes(st: &AppState, profile: &db::ProfileRow) -> Result<HashSet<String>, AppError> {
    let mut owned = HashSet::new();
    for id in owned_scope(st, profile).await? {
        owned.extend(db::hashes_by_backend(&st.pool, &id.to_string()).await?);
    }
    Ok(owned)
}

async fn load_profile(st: &AppState, profile_id: i64) -> Result<db::ProfileRow, AppError> {
    db::storage_profile(&st.pool, profile_id)
        .await?
        .ok_or_else(|| AppError::NotFound("存储档案不存在".into()))
}

/// 扫描阶段：列出该档案上的文件并分类，返回待删清单。
/// 过程中持续更新 total / done，让进度条真的动起来。
async fn scan_into(
    st: &AppState,
    profile: &db::ProfileRow,
    backend: &store::Backend,
) -> Result<Vec<String>, AppError> {
    let owned = owned_hashes(st, profile).await?;
    let mut all = Vec::new();
    'outer: for sub in ["images", "thumbs"] {
        if st.cleanup.cancel_flag() {
            break;
        }
        let names = backend.list(sub).await?;
        for name in names {
            if st.cleanup.cancel_flag() {
                break 'outer;
            }
            let orphan = match parse_name(&name) {
                // 名字不合本项目命名规则的文件不是自己写的，宁可留着
                None => false,
                Some((hash, _)) => !owned.contains(&hash),
            };
            let entry = format!("{sub}/{name}");
            let mut guard = st.cleanup.inner.lock().unwrap();
            let Some(r) = guard.as_mut() else { break 'outer };
            r.total += 1;
            r.done += 1;
            r.scanned += 1;
            if orphan {
                r.orphans += 1;
                if r.sample.len() < MAX_LISTED {
                    r.sample.push(entry.clone());
                }
                all.push(entry);
            } else {
                r.kept += 1;
            }
        }
    }
    Ok(all)
}

async fn run_scan(st: &AppState, profile_id: i64) -> Result<(), AppError> {
    let profile = load_profile(st, profile_id).await?;
    let backend = store::from_profile(&st.cfg.storage_dir, &profile)?;
    scan_into(st, &profile, &backend).await?;
    Ok(())
}

async fn run_delete(st: &AppState, profile_id: i64) -> Result<(), AppError> {
    let profile = load_profile(st, profile_id).await?;
    let backend = store::from_profile(&st.cfg.storage_dir, &profile)?;
    let all = scan_into(st, &profile, &backend).await?;
    if st.cleanup.cancel_flag() {
        return Ok(());
    }

    // 扫完进入删除阶段：进度计数改成「待删文件」的口径
    {
        let mut guard = st.cleanup.inner.lock().unwrap();
        let Some(r) = guard.as_mut() else { return Ok(()) };
        r.phase = Phase::Delete;
        r.total = all.len();
        r.done = 0;
    }

    let scope = owned_scope(st, &profile).await?;
    for entry in &all {
        if st.cleanup.cancel_flag() {
            break;
        }
        let (sub, name) = entry.split_once('/').unwrap_or(("images", entry));
        let Some((hash, _)) = parse_name(name) else { continue };
        // 逐个复核而不是只信扫描时的快照：扫描到删除之间可能有新上传写入同名
        // 文件并登记到本档案，那样它就不再是孤儿
        if db::hash_owned_by(&st.pool, &hash, &scope).await? {
            continue;
        }
        let result = backend.delete_checked(sub, name).await;
        let mut guard = st.cleanup.inner.lock().unwrap();
        let Some(r) = guard.as_mut() else { return Ok(()) };
        r.done += 1;
        match result {
            Ok(()) => r.deleted += 1,
            Err(e) => {
                r.failed += 1;
                if r.errors.len() < MAX_ERRORS {
                    r.errors.push(truncate(&format!("{entry}: {e}"), 300));
                }
            }
        }
    }

    // 删完清空清单：再显示一堆已经不存在的文件名只会让人误解。
    // orphans 保留原扫描值，界面用 deleted 表达「删了多少」、用 orphans 表达
    // 「当初发现多少」，两者相减就是没删掉的
    let mut guard = st.cleanup.inner.lock().unwrap();
    if let Some(r) = guard.as_mut() {
        r.sample.clear();
    }
    Ok(())
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}
