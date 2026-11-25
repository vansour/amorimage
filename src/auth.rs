use axum::extract::{Request, State};
use axum::http::request::Parts;
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::error::AppError;
use crate::state::AppState;

const COOKIE_NAME: &str = "amor_token";
const COOKIE_MAX_AGE: u32 = 30 * 24 * 3600;

#[derive(Deserialize)]
pub struct LoginBody {
    token: String,
}

fn token_matches(expected: &str, provided: &str) -> bool {
    // 哈希比较代替直接 ==，避免时序侧信道
    blake3::hash(provided.as_bytes()) == blake3::hash(expected.as_bytes())
}

fn cookie_value(parts: &Parts, name: &str) -> Option<String> {
    let header = parts.headers.get(header::COOKIE)?.to_str().ok()?;
    header.split(';').find_map(|pair| {
        let (k, v) = pair.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

fn is_authed(state: &AppState, parts: &Parts) -> bool {
    let Some(expected) = state.cfg.token.as_deref() else {
        return true;
    };
    if let Some(cookie) = cookie_value(parts, COOKIE_NAME)
        && token_matches(expected, &cookie)
    {
        return true;
    }
    if let Some(bearer) = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        && token_matches(expected, bearer)
    {
        return true;
    }
    false
}

pub async fn gate(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method();
    // 未配置令牌时保持开放模式；外链与探活永远公开。
    // 只按真实存在的路由形状 + 方法放行，避免空段路径或方法不符的请求
    // 落进 SPA 回退、把整份前端壳泄露给未认证者
    if state.cfg.token.is_none()
        || (matches!(*method, Method::GET | Method::HEAD)
            && (path == "/healthz"
                || path == "/favicon.ico"
                || crate::routes::is_public_asset(&path)))
        || (matches!(*method, Method::POST) && path == "/api/login")
    {
        return next.run(req).await;
    }

    let (parts, body) = req.into_parts();
    if is_authed(&state, &parts) {
        return next.run(Request::from_parts(parts, body)).await;
    }

    if path.starts_with("/api/") || path.contains('.') {
        return AppError::Unauthorized.into_response();
    }
    let title = crate::settings::stored_title(&state).await;
    let light = crate::settings::stored_theme(&state).await == "light";
    login_page(&title, light)
}

pub async fn login(State(st): State<AppState>, Json(body): Json<LoginBody>) -> Result<Response, AppError> {
    let Some(expected) = st.cfg.token.as_deref() else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let token = body.token.trim();
    if !token_matches(expected, token) {
        return Err(AppError::Unauthorized);
    }
    let cookie = format!(
        "{COOKIE_NAME}={token}; Path=/; Max-Age={COOKIE_MAX_AGE}; HttpOnly; SameSite=Lax"
    );
    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "ok": true })),
    )
        .into_response())
}

pub async fn logout() -> Response {
    let cookie = format!("{COOKIE_NAME}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax");
    ([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response()
}

fn login_page(title: &str, light: bool) -> Response {
    let body_class = if light { " class=\"light\"" } else { "" };
    let html = LOGIN_HTML
        .replace("%TITLE%", &html_escape(title))
        .replace("%BODYCLASS%", body_class);
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const LOGIN_HTML: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>%TITLE%</title>
<style>
:root{--bg:#181411;--surface:#201b16;--line:#373028;--text:#ede6da;--muted:#9c9083;--rose:#e85d75}
body.light{--bg:#f3efe8;--surface:#fbf8f2;--line:#d9d0c1;--text:#221c15;--muted:#8a7e6e;--rose:#c94b5e}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);font-family:ui-monospace,Menlo,Consolas,'PingFang SC','Microsoft YaHei',monospace;display:grid;place-items:center;min-height:100vh}
.box{width:min(330px,86vw);border:1px solid var(--line);background:var(--surface);padding:28px 24px}
.mark{width:10px;height:10px;background:var(--rose);display:inline-block;margin-right:10px;flex:none}
h1{margin:0 0 6px;font-size:18px;font-weight:700;letter-spacing:.5px;display:flex;align-items:center;overflow-wrap:anywhere}
p{margin:0 0 16px;color:var(--muted);font-size:12.5px}
input{width:100%;font:inherit;color:var(--text);background:var(--bg);border:1px solid var(--line);padding:9px 10px;border-radius:2px}
input:focus{outline:2px solid var(--rose);outline-offset:-1px}
button{width:100%;margin-top:12px;font:inherit;color:var(--rose);background:none;border:1px solid var(--rose);padding:9px;cursor:pointer;border-radius:2px}
button:hover{background:rgba(200,80,100,.12)}
.err{color:var(--rose);font-size:12px;margin-top:10px;display:none}
</style>
</head>
<body%BODYCLASS%>
<div class="box">
<h1><span class="mark"></span>%TITLE%</h1>
<p>这个图床是私有的，请输入访问令牌。</p>
<form id="f">
<input id="t" type="password" placeholder="访问令牌" autofocus autocomplete="current-password">
<button type="submit">进入</button>
<div class="err" id="e"></div>
</form>
</div>
<script>
document.getElementById('f').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const t = document.getElementById('t').value.trim();
  try {
    const r = await fetch('/api/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ token: t }),
    });
    if (r.ok) { location.replace('/'); return; }
  } catch {}
  document.getElementById('e').textContent = '令牌不对，再试一次。';
  document.getElementById('e').style.display = 'block';
});
</script>
</body>
</html>"#;
