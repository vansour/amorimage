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
    let has_favicon = matches!(db::settings_get(&st.pool, "favicon").await?, Some(_));
    Ok(Json(json!({
        "title": title,
        "theme": theme,
        "max_file_mb": max_file_mb,
        "max_body_mb": max_body_mb,
        "has_favicon": has_favicon,
    })))
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
