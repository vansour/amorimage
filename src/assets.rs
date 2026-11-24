use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "frontend/dist"]
struct Assets;

const INDEX: &str = "index.html";

pub async fn serve_static(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { INDEX } else { path };
    if let Some(file) = Assets::get(path) {
        return asset(path, &file.data);
    }
    // SPA 回退：不带扩展名的路径一律交给前端路由
    if !path.contains('.') {
        if let Some(file) = Assets::get(INDEX) {
            return asset(INDEX, &file.data);
        }
    }
    if Assets::get(INDEX).is_some() {
        // 前端已构建，只是这个资源不存在
        return (StatusCode::NOT_FOUND, "资源不存在").into_response();
    }
    (
        StatusCode::NOT_FOUND,
        "前端尚未构建：请先在 frontend/ 目录执行 npm run build（开发模式下请使用 Vite 开发服务器）",
    )
        .into_response()
}

fn asset(path: &str, data: &[u8]) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream().to_string();
    let cache = if path == INDEX {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        data.to_vec(),
    )
        .into_response()
}
