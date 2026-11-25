use axum::body::Bytes;
use axum::extract::{multipart::Multipart, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::{self, ImageRow, NewImage};
use crate::error::AppError;
use crate::imaging::{self, ProcessedImage};
use crate::state::AppState;
use crate::store;

const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";

#[derive(Serialize)]
pub struct ImageJson {
    #[serde(flatten)]
    row: ImageRow,
    url: String,
    thumb_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    link: Option<String>,
}

fn image_json(st: &AppState, row: ImageRow) -> ImageJson {
    let url = format!("/img/{}.{}", row.hash, row.ext);
    let thumb_url = format!("/thumb/{}.webp", row.hash);
    let link = st.cfg.base_url.as_ref().map(|base| format!("{base}{url}"));
    ImageJson { row, url, thumb_url, link }
}

// ---------- 上传 ----------

#[derive(Serialize)]
struct UploadItem {
    name: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<ImageJson>,
}

impl UploadItem {
    fn error(name: String, message: String) -> UploadItem {
        UploadItem { name, status: "error", message: Some(message), image: None }
    }

    fn duplicate(name: String, img: ImageJson) -> UploadItem {
        UploadItem {
            name,
            status: "duplicate",
            message: Some("内容已存在，未重复保存".into()),
            image: Some(img),
        }
    }
}

pub async fn upload(
    State(st): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    let max_file_bytes = crate::settings::stored_max_file_mb(&st)
        .await
        .map(|mb| mb * 1024 * 1024);
    let max_total_bytes = crate::settings::stored_max_body_mb(&st)
        .await
        .map(|mb| mb * 1024 * 1024);
    let mut total: u64 = 0;
    let mut results: Vec<UploadItem> = Vec::new();

    while let Some(mut field) = multipart.next_field().await? {
        let name = field.file_name().unwrap_or("unnamed").to_string();
        let is_file = matches!(field.name(), Some("files") | Some("file"));
        // 流式读取：累计超限立即停止缓冲，字段不再先整个读进内存
        let mut data: Vec<u8> = Vec::new();
        let mut field_error: Option<String> = None;
        loop {
            let chunk = match field.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(e) => {
                    field_error = Some(format!("读取数据失败: {e}"));
                    break;
                }
            };
            total += chunk.len() as u64;
            if let Some(cap) = max_total_bytes && total > cap {
                return Err(AppError::PayloadTooLarge(format!(
                    "请求总量超过 {}MB 上限，本次上传已中止",
                    cap / 1024 / 1024
                )));
            }
            if !is_file {
                continue; // 非文件字段只计入总量，不缓冲
            }
            if let Some(cap) = max_file_bytes && data.len() + chunk.len() > cap as usize {
                // 超出单文件上限：停止缓冲，但把该字段读完以维持流的一致性
                field_error = Some(format!("超过单文件大小限制 {}MB", cap / 1024 / 1024));
                continue;
            }
            data.extend_from_slice(&chunk);
        }
        if let Some(msg) = field_error {
            results.push(UploadItem::error(name, msg));
            continue;
        }
        if !is_file {
            continue;
        }
        total += 256;
        match store_one(&st, &name, Bytes::from(data)).await {
            Ok(item) => results.push(item),
            Err(msg) => results.push(UploadItem::error(name, msg)),
        }
    }

    Ok(Json(json!({ "results": results })))
}

async fn store_one(st: &AppState, name: &str, data: Bytes) -> Result<UploadItem, String> {
    let hash = blake3::hash(&data).to_hex().to_string();

    if let Ok(Some(existing)) = db::find_by_hash(&st.pool, &hash).await {
        return Ok(UploadItem::duplicate(name.to_string(), image_json(st, existing)));
    }

    // 解码/重编码是 CPU 密集操作，放到阻塞线程池，避免占住 tokio worker
    let task_data = data.clone();
    let proc: ProcessedImage = tokio::task::spawn_blocking(move || imaging::process(&task_data))
        .await
        .map_err(|e| format!("图片处理任务失败: {e}"))?
        .map_err(|e| e.to_string())?;
    let file_name = format!("{hash}.{}", proc.ext);
    let thumb_name = format!("{hash}.webp");
    let size = proc.bytes.len() as u64;
    let thumb_size = proc.thumb.len() as u64;
    let backend = store::backend(st).await.map_err(|e| e.to_string())?;

    backend
        .put("images", &file_name, proc.bytes.into())
        .await
        .map_err(|e| e.to_string())?;
    if let Err(e) = backend.put("thumbs", &thumb_name, proc.thumb.into()).await {
        backend.delete("images", &file_name).await;
        return Err(e.to_string());
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("系统时间异常: {e}"))?
        .as_millis() as i64;
    let backend_name = store::backend_name(st).await;
    let new = NewImage {
        hash: &hash,
        orig_name: name,
        ext: proc.ext,
        mime: proc.mime,
        width: proc.width,
        height: proc.height,
        size,
        thumb_size,
        created_at: now,
        backend: &backend_name,
    };
    let id = match db::insert(&st.pool, &new).await {
        Ok(id) => id,
        Err(e) => {
            // 并发上传同一内容触发 UNIQUE 约束时按已存在处理
            if let Ok(Some(existing)) = db::find_by_hash(&st.pool, &hash).await {
                return Ok(UploadItem::duplicate(name.to_string(), image_json(st, existing)));
            }
            return Err(format!("数据库写入失败: {e}"));
        }
    };

    let row = ImageRow {
        id,
        hash,
        orig_name: name.to_string(),
        ext: proc.ext.to_string(),
        mime: proc.mime.to_string(),
        width: proc.width as i64,
        height: proc.height as i64,
        size: size as i64,
        thumb_size: thumb_size as i64,
        created_at: now,
        backend: backend_name,
    };
    Ok(UploadItem {
        name: name.to_string(),
        status: "ok",
        message: None,
        image: Some(image_json(st, row)),
    })
}

// ---------- 列表 / 删除 ----------

#[derive(Deserialize)]
pub struct ListQuery {
    limit: Option<i64>,
    before: Option<i64>,
}

pub async fn list_images(
    State(st): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let limit = q.limit.unwrap_or(60).clamp(1, 200);
    let before = q.before.filter(|b| *b > 0);
    let total = db::count(&st.pool).await?;
    let rows = db::list(&st.pool, limit, before).await?;
    let items: Vec<ImageJson> = rows.into_iter().map(|r| image_json(&st, r)).collect();
    Ok(Json(json!({ "total": total, "items": items })))
}

pub async fn delete_image(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    let row = db::find_by_id(&st.pool, id)
        .await?
        .ok_or_else(|| AppError::NotFound("图片不存在".into()))?;
    // 先删记录再删文件：文件删除失败最多留孤儿文件（可清理），
    // 反过来会在删库失败时留下「列表里有、点开 404」的坏记录
    db::delete(&st.pool, id).await?;
    let backend = store::backend_for(&st, &row.backend).await?;
    backend.delete("images", &format!("{}.{}", row.hash, row.ext)).await;
    backend.delete("thumbs", &format!("{}.webp", row.hash)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- 图片服务 ----------

fn parse_name(fname: &str) -> Option<(String, String)> {
    let (hash, ext) = fname.split_once('.')?;
    let valid_hash = !hash.is_empty()
        && hash.len() <= 64
        && hash.bytes().all(|b| b.is_ascii_hexdigit());
    let valid_ext =
        !ext.is_empty() && ext.len() <= 5 && ext.bytes().all(|b| b.is_ascii_alphanumeric());
    if valid_hash && valid_ext {
        Some((hash.to_string(), ext.to_string()))
    } else {
        None
    }
}

/// /img/、/thumb/ 下符合真实路由参数形状的名字；auth gate 按此放行公开外链
pub(crate) fn is_public_asset(path: &str) -> bool {
    let name = path
        .strip_prefix("/img/")
        .or_else(|| path.strip_prefix("/thumb/"))
        .unwrap_or("");
    parse_name(name).is_some()
}

async fn serve_file(st: &AppState, sub: &str, fname: &str, mime: &str) -> Result<Response, AppError> {
    // 文件可能停在旧后端上：先看记录里记的后端，旧数据才回落到当前设置
    let hash = fname.rsplit_once('.').map(|(h, _)| h).unwrap_or(fname);
    let backend = match db::find_by_hash(&st.pool, hash).await? {
        Some(row) => store::backend_for(st, &row.backend).await?,
        None => store::backend(st).await?,
    };
    let data = backend.get(sub, fname).await?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::CACHE_CONTROL, IMMUTABLE_CACHE.to_string()),
        ],
        data,
    )
        .into_response())
}

pub async fn serve_image(
    State(st): State<AppState>,
    Path(fname): Path<String>,
) -> Result<Response, AppError> {
    let (_, ext) = parse_name(&fname)
        .ok_or_else(|| AppError::BadRequest("非法文件名".into()))?;
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return Err(AppError::BadRequest("非法文件名".into())),
    };
    serve_file(&st, "images", &fname, mime).await
}

pub async fn serve_thumb(
    State(st): State<AppState>,
    Path(fname): Path<String>,
) -> Result<Response, AppError> {
    let (_, ext) = parse_name(&fname)
        .ok_or_else(|| AppError::BadRequest("非法文件名".into()))?;
    if ext != "webp" {
        return Err(AppError::BadRequest("非法文件名".into()));
    }
    serve_file(&st, "thumbs", &fname, "image/webp").await
}
