use std::io::Cursor;

use axum::extract::{Multipart, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::cleanup;
use crate::db;
use crate::error::AppError;
use crate::migrate;
use crate::state::AppState;
use crate::store;

const MAX_FAVICON_BYTES: usize = 10 * 1024 * 1024;

/// MB 上限取值边界：上限值会做 `mb * 1024 * 1024` 乘法，超过 2^44 会回绕 u64，
/// 1TB 以上的取值也没有实际意义
const MAX_MB: u64 = 1 << 20;

pub async fn get(State(st): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let title = stored_title(&st).await;
    let theme = stored_theme(&st).await;
    let max_file_mb = stored_max_file_mb(&st).await;
    let max_body_mb = stored_max_body_mb(&st).await;
    let has_favicon = db::settings_get(&st.pool, "favicon").await?.is_some();
    let storage = storage_settings(&st).await?;
    Ok(Json(json!({
        "title": title,
        "theme": theme,
        "max_file_mb": max_file_mb,
        "max_body_mb": max_body_mb,
        "has_favicon": has_favicon,
        "storage": storage,
    })))
}

/// 档案配置里密钥只回传「是否已设置」，避免从接口把密钥读走
async fn storage_settings(st: &AppState) -> Result<serde_json::Value, AppError> {
    let active = store::active_profile_id(st).await?;
    let rows = db::storage_profiles(&st.pool).await?;
    let mut profiles = Vec::with_capacity(rows.len());
    for row in &rows {
        profiles.push(profile_json(st, row).await);
    }
    Ok(json!({
        "active": active,
        "profiles": profiles,
        "migration": st.migrate.progress(),
        "pending": db::count_pending_move(&st.pool, active).await?,
    }))
}

async fn profile_json(st: &AppState, row: &db::ProfileRow) -> serde_json::Value {
    let cfg: serde_json::Value = serde_json::from_str(&row.config).unwrap_or_default();
    let text = |key: &str| cfg.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let count = db::count_by_backend(&st.pool, &row.id.to_string()).await.unwrap_or(0);
    json!({
        "id": row.id,
        "name": row.name,
        "kind": row.kind,
        "images": count,
        "webdav_url": text("url"),
        "webdav_username": text("username"),
        "webdav_has_password": !text("password").is_empty(),
        "s3_endpoint": text("endpoint"),
        "s3_bucket": text("bucket"),
        "s3_region": text("region"),
        "s3_access_key": text("access_key"),
        "s3_has_secret": !text("secret_key").is_empty(),
        "s3_path_style": cfg.get("path_style").and_then(|v| v.as_bool()).unwrap_or(true),
    })
}

pub async fn stored_title(st: &AppState) -> String {
    db::settings_get(&st.pool, "title")
        .await
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v).ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "amorimage".into())
}

pub async fn stored_theme(st: &AppState) -> &'static str {
    match db::settings_get(&st.pool, "theme").await {
        Ok(Some(v)) if v == b"light" => "light",
        _ => "dark",
    }
}

/// None 表示无限制（未设置或显式存 0）
pub async fn stored_max_file_mb(st: &AppState) -> Option<u64> {
    db::settings_get(&st.pool, "max_file_mb")
        .await
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(|mb| mb.min(MAX_MB))
        .filter(|mb| *mb > 0)
}

/// None 表示无限制（显式存 0），未设置时默认 1024MB
pub async fn stored_max_body_mb(st: &AppState) -> Option<u64> {
    db::settings_get(&st.pool, "max_body_mb")
        .await
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|v| v.parse::<u64>().ok())
        .or(Some(1024))
        .map(|mb| mb.min(MAX_MB))
        .filter(|mb| *mb > 0)
}

#[derive(Deserialize)]
pub struct SettingsPatch {
    title: Option<String>,
    theme: Option<String>,
    max_file_mb: Option<u64>,
    max_body_mb: Option<u64>,
    /// 切换当前存储档案
    active_profile: Option<i64>,
}

pub async fn update(
    State(st): State<AppState>,
    Json(p): Json<SettingsPatch>,
) -> Result<StatusCode, AppError> {
    if let Some(title) = &p.title {
        let title = title.trim();
        if title.chars().count() > 60 {
            return Err(AppError::BadRequest("标题过长，最多 60 个字符".into()));
        }
        db::settings_set(&st.pool, "title", title.as_bytes()).await?;
    }
    if let Some(theme) = &p.theme {
        if !matches!(theme.as_str(), "dark" | "light") {
            return Err(AppError::BadRequest("主题只能是 dark 或 light".into()));
        }
        db::settings_set(&st.pool, "theme", theme.as_bytes()).await?;
    }
    if let Some(mb) = p.max_file_mb {
        if mb > MAX_MB {
            return Err(AppError::BadRequest(format!("单文件上限过大，最多 {MAX_MB}MB")));
        }
        // 0 表示无限制
        db::settings_set(&st.pool, "max_file_mb", mb.to_string().as_bytes()).await?;
    }
    if let Some(mb) = p.max_body_mb {
        if mb > MAX_MB {
            return Err(AppError::BadRequest(format!("请求总量上限过大，最多 {MAX_MB}MB")));
        }
        // 0 表示无限制
        db::settings_set(&st.pool, "max_body_mb", mb.to_string().as_bytes()).await?;
    }
    if let Some(id) = p.active_profile {
        if db::storage_profile(&st.pool, id).await?.is_none() {
            return Err(AppError::BadRequest("存储档案不存在".into()));
        }
        db::settings_set(&st.pool, "storage_profile", id.to_string().as_bytes()).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- 存储档案 ----------

#[derive(Deserialize)]
pub struct ProfileBody {
    /// 缺省表示新建
    id: Option<i64>,
    name: String,
    kind: String,
    webdav_url: Option<String>,
    webdav_username: Option<String>,
    /// 留空表示保留原值
    webdav_password: Option<String>,
    s3_endpoint: Option<String>,
    s3_bucket: Option<String>,
    s3_region: Option<String>,
    s3_access_key: Option<String>,
    s3_secret_key: Option<String>,
    s3_path_style: Option<bool>,
}

impl ProfileBody {
    /// 合并已有档案里的密钥：前端不会回显它们，留空即保持不变
    fn config_json(&self, existing: Option<&serde_json::Value>) -> Result<String, AppError> {
        let old = |key: &str| {
            existing
                .and_then(|v| v.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let text = |v: &Option<String>| v.as_deref().unwrap_or_default().trim().to_string();
        let secret = |v: &Option<String>, key: &str| {
            v.as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| old(key))
        };
        let require_http = |value: &str, label: &str| -> Result<(), AppError> {
            if !value.is_empty()
                && !(value.starts_with("https://") || value.starts_with("http://"))
            {
                return Err(AppError::BadRequest(format!("{label} 必须以 http:// 或 https:// 开头")));
            }
            Ok(())
        };
        let cfg = match self.kind.as_str() {
            "local" => serde_json::json!({}),
            "webdav" => {
                let url = text(&self.webdav_url);
                require_http(&url, "WebDAV 地址")?;
                serde_json::json!({
                    "url": url,
                    "username": text(&self.webdav_username),
                    "password": secret(&self.webdav_password, "password"),
                })
            }
            "s3" => {
                let endpoint = text(&self.s3_endpoint);
                require_http(&endpoint, "S3 Endpoint")?;
                let bucket = text(&self.s3_bucket);
                if bucket.contains(['/', '\\', ' ']) {
                    return Err(AppError::BadRequest("存储桶名称不能包含斜杠或空格".into()));
                }
                serde_json::json!({
                    "endpoint": endpoint,
                    "bucket": bucket,
                    "region": text(&self.s3_region),
                    "access_key": text(&self.s3_access_key),
                    "secret_key": secret(&self.s3_secret_key, "secret_key"),
                    "path_style": self.s3_path_style.unwrap_or_else(|| {
                        existing
                            .and_then(|v| v.get("path_style"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true)
                    }),
                })
            }
            _ => return Err(AppError::BadRequest("存储类型只能是 local / webdav / s3".into())),
        };
        Ok(cfg.to_string())
    }
}

pub async fn save_profile(
    State(st): State<AppState>,
    Json(body): Json<ProfileBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("请填写档案名称".into()));
    }
    if name.chars().count() > 40 {
        return Err(AppError::BadRequest("档案名称最多 40 个字符".into()));
    }
    let existing = match body.id {
        Some(id) => {
            let row = db::storage_profile(&st.pool, id)
                .await?
                .ok_or_else(|| AppError::NotFound("存储档案不存在".into()))?;
            if row.kind != body.kind {
                return Err(AppError::BadRequest("不能修改已有档案的存储类型".into()));
            }
            Some(serde_json::from_str::<serde_json::Value>(&row.config).unwrap_or_default())
        }
        None => None,
    };
    let config = body.config_json(existing.as_ref())?;
    let id = match body.id {
        Some(id) => {
            db::storage_profile_update(&st.pool, id, name, &config).await?;
            id
        }
        None => db::storage_profile_insert(&st.pool, name, &body.kind, &config).await?,
    };
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct ProfileQuery {
    id: i64,
}

pub async fn delete_profile(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ProfileQuery>,
) -> Result<StatusCode, AppError> {
    let id = q.id;
    let active = store::active_profile_id(&st).await?;
    if id == active {
        return Err(AppError::BadRequest(
            "不能删除正在使用的档案，请先切换到别的档案".into(),
        ));
    }
    let stored = db::count_by_backend(&st.pool, &id.to_string()).await?;
    if stored > 0 {
        return Err(AppError::BadRequest(format!(
            "该档案上还有 {stored} 张图片，请先迁移到别的档案再删除"
        )));
    }
    db::storage_profile_delete(&st.pool, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ProbeBody {
    /// 试连已保存的档案
    id: Option<i64>,
    /// 试连尚未保存的配置
    profile: Option<ProfileBody>,
}

/// 用请求里带的配置或已存档案直接试连，不必先保存
pub async fn probe_storage(
    State(st): State<AppState>,
    Json(body): Json<ProbeBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (kind, config) = match (body.id, body.profile) {
        (Some(id), _) => {
            let row = db::storage_profile(&st.pool, id)
                .await?
                .ok_or_else(|| AppError::NotFound("存储档案不存在".into()))?;
            (row.kind, row.config)
        }
        (None, Some(p)) => {
            // 编辑已有档案时密钥框是空的：沿用已保存的值，否则试连必然失败
            let existing = match p.id {
                Some(id) => db::storage_profile(&st.pool, id)
                    .await?
                    .and_then(|row| serde_json::from_str::<serde_json::Value>(&row.config).ok()),
                None => None,
            };
            (p.kind.clone(), p.config_json(existing.as_ref())?)
        }
        (None, None) => return Err(AppError::BadRequest("缺少要测试的存储配置".into())),
    };
    let row = db::ProfileRow {
        id: 0,
        name: String::new(),
        kind: kind.clone(),
        config,
    };
    let backend = store::from_profile(&st.cfg.storage_dir, &row)?;
    match backend.probe().await {
        Ok(()) => Ok(Json(json!({ "ok": true, "message": "连接成功" }))),
        Err(e) => Ok(Json(json!({ "ok": false, "message": e }))),
    }
}

#[derive(Deserialize)]
pub struct MigrateBody {
    /// 目标档案；缺省为当前档案
    target: Option<i64>,
    /// 成功搬完后删掉源文件
    #[serde(default)]
    delete_source: bool,
}

pub async fn start_migration(
    State(st): State<AppState>,
    Json(body): Json<MigrateBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let target = match body.target {
        Some(id) => id,
        None => store::active_profile_id(&st).await?,
    };
    migrate::start(&st, target, body.delete_source).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn migration_progress(
    State(st): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let active = store::active_profile_id(&st).await?;
    Ok(Json(json!({
        "migration": st.migrate.progress(),
        "pending": db::count_pending_move(&st.pool, active).await?,
    })))
}

pub async fn cancel_migration(
    State(st): State<AppState>,
) -> Result<StatusCode, AppError> {
    if !st.migrate.cancel() {
        return Err(AppError::BadRequest("没有正在进行的迁移任务".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct CleanupBody {
    /// 要清理的档案；缺省为当前档案
    profile: Option<i64>,
    /// true 才真正删除；默认只预演
    #[serde(default)]
    confirm: bool,
}

async fn cleanup_target(st: &AppState, profile: Option<i64>) -> Result<i64, AppError> {
    match profile {
        Some(id) => Ok(id),
        None => store::active_profile_id(st).await,
    }
}

/// 预演：后台扫描并统计该档案上的孤儿文件，不删任何东西
pub async fn scan_orphans(
    State(st): State<AppState>,
    Json(body): Json<CleanupBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = cleanup_target(&st, body.profile).await?;
    cleanup::start_scan(&st, id).await?;
    Ok(Json(json!({ "profile": id, "started": true })))
}

/// 真正删除孤儿。前端必须在预演后二次确认才会走到这里
pub async fn delete_orphans(
    State(st): State<AppState>,
    Json(body): Json<CleanupBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !body.confirm {
        return Err(AppError::BadRequest("缺少确认标记，未删除任何文件".into()));
    }
    let id = cleanup_target(&st, body.profile).await?;
    cleanup::start_delete(&st, id).await?;
    Ok(Json(json!({ "profile": id, "started": true })))
}

pub async fn cleanup_progress(
    State(st): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(json!({ "cleanup": st.cleanup.progress() })))
}

pub async fn cancel_cleanup(State(st): State<AppState>) -> Result<StatusCode, AppError> {
    if !st.cleanup.cancel() {
        return Err(AppError::BadRequest("没有正在进行的清理任务".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn upload_favicon(
    State(st): State<AppState>,
    mut multipart: Multipart,
) -> Result<StatusCode, AppError> {
    let field = multipart
        .next_field()
        .await?
        .ok_or_else(|| AppError::BadRequest("缺少文件字段".into()))?;
    if field.name() != Some("file") {
        return Err(AppError::BadRequest("文件字段名必须是 file".into()));
    }
    let data = field.bytes().await?;
    if data.len() > MAX_FAVICON_BYTES {
        return Err(AppError::BadRequest("图标超过 10MB 限制".into()));
    }

    let format = image::ImageReader::new(Cursor::new(&data))
        .with_guessed_format()
        .map_err(|e| AppError::BadRequest(format!("无法识别文件: {e}")))?
        .format()
        .ok_or_else(|| AppError::BadRequest("无法识别文件类型".into()))?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        image::ImageFormat::Ico => "image/x-icon",
        _ => return Err(AppError::BadRequest("仅支持 PNG / JPEG / GIF / WebP / ICO".into())),
    };
    // 解码校验是 CPU 密集操作，放到阻塞线程池，避免占住 tokio worker
    let check = data.clone();
    tokio::task::spawn_blocking(move || image::load_from_memory_with_format(&check, format))
        .await
        .map_err(|e| AppError::Internal(format!("图标校验任务失败: {e}")))?
        .map_err(|_| AppError::BadRequest("不是有效的图片文件".into()))?;

    db::settings_set(&st.pool, "favicon", &data).await?;
    db::settings_set(&st.pool, "favicon_mime", mime.as_bytes()).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_favicon(State(st): State<AppState>) -> Result<StatusCode, AppError> {
    db::settings_delete(&st.pool, "favicon").await?;
    db::settings_delete(&st.pool, "favicon_mime").await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn serve_favicon(State(st): State<AppState>) -> Result<Response, AppError> {
    let Some(data) = db::settings_get(&st.pool, "favicon").await? else {
        return Err(AppError::NotFound("未设置图标".into()));
    };
    let mime = db::settings_get(&st.pool, "favicon_mime")
        .await?
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_else(|| "image/x-icon".into());
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            // favicon 会更换，短缓存加 must-revalidate 避免标签页长期显示旧图标
            (header::CACHE_CONTROL, "public, max-age=300, must-revalidate".to_string()),
        ],
        data,
    )
        .into_response())
}
