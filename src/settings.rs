use std::io::Cursor;

use axum::extract::{Multipart, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::db;
use crate::error::AppError;
use crate::state::AppState;

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
    let storage = storage_settings(&st).await;
    Ok(Json(json!({
        "title": title,
        "theme": theme,
        "max_file_mb": max_file_mb,
        "max_body_mb": max_body_mb,
        "has_favicon": has_favicon,
        "storage": storage,
    })))
}

/// 存储配置里密钥只回传「是否已设置」，避免从接口把密钥读走
async fn storage_settings(st: &AppState) -> serde_json::Value {
    let text = |key: &'static str| {
        let st = st.clone();
        async move { setting(&st, key).await.unwrap_or_default() }
    };
    json!({
        "backend": setting(st, "storage_backend").await.unwrap_or_else(|| "local".into()),
        "webdav_url": text("webdav_url").await,
        "webdav_username": text("webdav_username").await,
        "webdav_has_password": db::settings_get(&st.pool, "webdav_password").await.ok().flatten().is_some_and(|v| !v.is_empty()),
        "s3_endpoint": text("s3_endpoint").await,
        "s3_bucket": text("s3_bucket").await,
        "s3_region": text("s3_region").await,
        "s3_access_key": text("s3_access_key").await,
        "s3_has_secret": db::settings_get(&st.pool, "s3_secret_key").await.ok().flatten().is_some_and(|v| !v.is_empty()),
        "s3_path_style": setting(st, "s3_path_style").await.as_deref() != Some("0"),
    })
}

async fn setting(st: &AppState, key: &'static str) -> Option<String> {
    db::settings_get(&st.pool, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v).ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
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
    #[serde(default)]
    storage: Option<StoragePatch>,
}

#[derive(Deserialize)]
pub struct StoragePatch {
    backend: Option<String>,
    webdav_url: Option<String>,
    webdav_username: Option<String>,
    /// 传空字符串表示保留原密码，单独传 clear 才清空
    webdav_password: Option<String>,
    s3_endpoint: Option<String>,
    s3_bucket: Option<String>,
    s3_region: Option<String>,
    s3_access_key: Option<String>,
    s3_secret_key: Option<String>,
    s3_path_style: Option<bool>,
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
    if let Some(s) = &p.storage {
        update_storage(&st, s).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn update_storage(st: &AppState, s: &StoragePatch) -> Result<(), AppError> {
    if let Some(backend) = &s.backend {
        if !matches!(backend.as_str(), "local" | "webdav" | "s3") {
            return Err(AppError::BadRequest("存储后端只能是 local / webdav / s3".into()));
        }
        db::settings_set(&st.pool, "storage_backend", backend.as_bytes()).await?;
    }

    // 地址类配置必须是 http(s)，否则后端每条请求都会失败
    for (key, value) in [
        ("webdav_url", &s.webdav_url),
        ("s3_endpoint", &s.s3_endpoint),
    ] {
        if let Some(url) = value {
            let url = url.trim();
            if !url.is_empty() && !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(AppError::BadRequest(format!("{key} 必须以 http:// 或 https:// 开头")));
            }
            db::settings_set(&st.pool, key, url.as_bytes()).await?;
        }
    }
    for (key, value) in [
        ("webdav_username", &s.webdav_username),
        ("s3_bucket", &s.s3_bucket),
        ("s3_region", &s.s3_region),
        ("s3_access_key", &s.s3_access_key),
    ] {
        if let Some(v) = value {
            let v = v.trim();
            if key == "s3_bucket" && v.contains(['/', '\\', ' ']) {
                return Err(AppError::BadRequest("存储桶名称不能包含斜杠或空格".into()));
            }
            db::settings_set(&st.pool, key, v.as_bytes()).await?;
        }
    }
    // 密钥留空表示不改动，避免前端回显密钥的往返把它们清空
    if let Some(pw) = s.webdav_password.as_ref().filter(|v| !v.is_empty()) {
        db::settings_set(&st.pool, "webdav_password", pw.as_bytes()).await?;
    }
    if let Some(secret) = s.s3_secret_key.as_ref().filter(|v| !v.is_empty()) {
        db::settings_set(&st.pool, "s3_secret_key", secret.as_bytes()).await?;
    }
    if let Some(path_style) = s.s3_path_style {
        db::settings_set(&st.pool, "s3_path_style", if path_style { b"1" } else { b"0" }).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct ProbeBody {
    storage: StoragePatch,
}

/// 用请求里带的配置直接试连，不必先保存——避免把错配置落库、影响正在用的上传
pub async fn probe_storage(
    State(st): State<AppState>,
    Json(body): Json<ProbeBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let s = &body.storage;
    let backend = s
        .backend
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("缺少存储后端类型".into()))?;
    let saved = |key: &'static str| {
        let st = st.clone();
        async move { setting(&st, key).await }
    };
    let backend = match backend {
        "local" => {
            return Ok(Json(json!({ "ok": true, "message": "本地磁盘可用" })));
        }
        "webdav" => {
            let url = s.webdav_url.as_deref().map(str::trim).filter(|v| !v.is_empty());
            let url = match url {
                Some(v) => v.to_string(),
                None => saved("webdav_url").await.unwrap_or_default(),
            };
            if url.is_empty() {
                return Err(AppError::BadRequest("请先填写 WebDAV 地址".into()));
            }
            let username = s
                .webdav_username
                .as_deref()
                .map(str::trim)
                .map(str::to_string)
                .or(saved("webdav_username").await)
                .unwrap_or_default();
            let password = s
                .webdav_password
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .or(saved("webdav_password").await)
                .unwrap_or_default();
            crate::store::Backend::WebDav(crate::store::WebDavConfig::new(url, username, password))
        }
        "s3" => {
            let endpoint = match s.s3_endpoint.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                Some(v) => v.to_string(),
                None => saved("s3_endpoint").await.unwrap_or_default(),
            };
            let bucket = match s.s3_bucket.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                Some(v) => v.to_string(),
                None => saved("s3_bucket").await.unwrap_or_default(),
            };
            let region = s
                .s3_region
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .or(saved("s3_region").await)
                .unwrap_or_default();
            let access_key = match s.s3_access_key.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                Some(v) => v.to_string(),
                None => saved("s3_access_key").await.unwrap_or_default(),
            };
            let secret_key = s
                .s3_secret_key
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .or(saved("s3_secret_key").await)
                .unwrap_or_default();
            let path_style = match s.s3_path_style {
                Some(v) => v,
                None => saved("s3_path_style").await.as_deref() != Some("0"),
            };
            crate::store::Backend::S3(crate::store::S3Config::new(
                endpoint, bucket, region, access_key, secret_key, path_style,
            ))
        }
        _ => return Err(AppError::BadRequest("存储后端只能是 local / webdav / s3".into())),
    };
    match backend.probe().await {
        Ok(()) => Ok(Json(json!({ "ok": true, "message": "连接成功" }))),
        Err(e) => Ok(Json(json!({ "ok": false, "message": e }))),
    }
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
