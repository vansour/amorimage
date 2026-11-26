//! 把已存在的图片从一个存储档案搬到另一个。
//!
//! 后台任务逐条处理：先从原档案读出、写入目标档案，成功后才更新记录的
//! 档案 id。因此中途失败、取消或进程重启都不会丢数据——记录仍指向原档案。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::db;
use crate::error::AppError;
use crate::state::AppState;
use crate::store;

/// 每批取多少条；批之间落锁一次，取消能及时生效
const BATCH: i64 = 20;
/// 进度里最多保留多少条失败原因，避免内存无限增长
const MAX_ERRORS: usize = 20;

#[derive(Default)]
pub struct MigrateState {
    inner: Mutex<Option<Run>>,
}

struct Run {
    target: i64,
    total: i64,
    done: i64,
    moved: i64,
    skipped: i64,
    failed: i64,
    finished: bool,
    cancelled: bool,
    errors: Vec<String>,
    cancel: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
pub struct Progress {
    /// 是否有任务在跑
    pub running: bool,
    /// 是否跑过一次（无论成败）
    pub present: bool,
    pub target: i64,
    pub total: i64,
    pub done: i64,
    pub moved: i64,
    pub skipped: i64,
    pub failed: i64,
    pub cancelled: bool,
    pub errors: Vec<String>,
}

impl MigrateState {
    pub fn new() -> MigrateState {
        MigrateState::default()
    }

    pub fn progress(&self) -> Progress {
        match self.inner.lock().unwrap().as_ref() {
            None => Progress {
                running: false,
                present: false,
                target: 0,
                total: 0,
                done: 0,
                moved: 0,
                skipped: 0,
                failed: 0,
                cancelled: false,
                errors: Vec::new(),
            },
            Some(r) => Progress {
                running: !r.finished,
                present: true,
                target: r.target,
                total: r.total,
                done: r.done,
                moved: r.moved,
                skipped: r.skipped,
                failed: r.failed,
                cancelled: r.cancelled,
                errors: r.errors.clone(),
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

/// 启动一次迁移；已有任务在跑时拒绝，避免两条流水线同时改同一批记录
pub async fn start(st: &AppState, target: i64, delete_source: bool) -> Result<(), AppError> {
    if db::storage_profile(&st.pool, target).await?.is_none() {
        return Err(AppError::BadRequest("目标存储档案不存在".into()));
    }
    // 清理正在删文件，此时把文件搬走会让它删到一半的清单失去意义
    if st.cleanup.is_running() {
        return Err(AppError::BadRequest(
            "孤儿文件清理正在进行，请等它结束后再迁移".into(),
        ));
    }
    let mut guard = st.migrate.inner.lock().unwrap();
    if guard.as_ref().is_some_and(|r| !r.finished) {
        return Err(AppError::BadRequest("已有迁移任务在进行中".into()));
    }
    *guard = Some(Run {
        target,
        total: 0,
        done: 0,
        moved: 0,
        skipped: 0,
        failed: 0,
        finished: false,
        cancelled: false,
        errors: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
    });
    drop(guard);

    let st = st.clone();
    tokio::spawn(async move {
        if let Err(e) = run(&st, target, delete_source).await {
            tracing::error!("存储迁移中止: {e}");
            finish(&st, Some(format!("迁移中止: {e}")), false);
        } else {
            finish(&st, None, st.migrate.cancel_flag());
        }
    });
    Ok(())
}

fn finish(st: &AppState, error: Option<String>, cancelled: bool) {
    let mut guard = st.migrate.inner.lock().unwrap();
    if let Some(r) = guard.as_mut() {
        if let Some(e) = error {
            r.errors.push(e);
        }
        r.finished = true;
        r.cancelled = cancelled;
    }
}

async fn run(st: &AppState, target: i64, delete_source: bool) -> Result<(), AppError> {
    let target_id = target.to_string();
    // 待迁移集合按「目标档案之外」算；跑之前定下来，避免边搬边变
    let total = db::count_pending_move(&st.pool, target).await?;
    let target_backend = store::backend_for(st, &target_id).await?;
    {
        let mut guard = st.migrate.inner.lock().unwrap();
        let Some(r) = guard.as_mut() else { return Ok(()) };
        r.total = total;
    }

    let mut last_id = 0i64;
    loop {
        if st.migrate.cancel_flag() {
            break;
        }
        let rows = db::list_pending_move(&st.pool, target, last_id, BATCH).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            if st.migrate.cancel_flag() {
                break;
            }
            last_id = row.id;
            let result =
                move_one(st, &row, &target_backend, &target_id, delete_source).await;
            let mut guard = st.migrate.inner.lock().unwrap();
            let Some(r) = guard.as_mut() else { return Ok(()) };
            r.done += 1;
            match result {
                Ok(()) => r.moved += 1,
                Err(e) => {
                    r.failed += 1;
                    if r.errors.len() < MAX_ERRORS {
                        let msg = format!(
                            "{} ({}.{}): {e}",
                            row.orig_name, row.hash, row.ext
                        );
                        r.errors.push(truncate(&msg, 300));
                    }
                }
            }
        }
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

async fn move_one(
    st: &AppState,
    row: &db::ImageRow,
    target: &store::Backend,
    target_id: &str,
    delete_source: bool,
) -> Result<(), AppError> {
    let source = store::backend_for(st, &row.backend).await?;
    let image = format!("{}.{}", row.hash, row.ext);
    let thumb = format!("{}.webp", row.hash);

    // 源文件读不到就记失败、保留原档案指向：把记录改到目标档案会让旧图彻底失去定位
    let image_bytes = source.get("images", &image).await?;
    target.put("images", &image, image_bytes).await?;

    // 缩略图缺失或写失败不致命：原图已在新位置，重新生成不了也只是图库预览降级
    let thumb_copied = match source.get("thumbs", &thumb).await {
        Ok(bytes) => match target.put("thumbs", &thumb, bytes).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("迁移缩略图 {thumb} 失败: {e}");
                false
            }
        },
        Err(e) => {
            tracing::warn!("读取源缩略图 {thumb} 失败: {e}");
            false
        }
    };

    db::set_backend(&st.pool, row.id, target_id).await?;

    if delete_source {
        source.delete("images", &image).await;
        if thumb_copied {
            source.delete("thumbs", &thumb).await;
        }
    }
    Ok(())
}
