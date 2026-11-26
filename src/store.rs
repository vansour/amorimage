//! 图片文件的落盘后端：本地磁盘、WebDAV、S3（含各类兼容实现）。
//!
//! 后端配置存在 SQLite 设置表里，每次上传/读取时现读现用，设置页改完立即生效。

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};

use crate::db;
use crate::error::AppError;
use crate::state::AppState;

/// 对象键前缀：存储桶可能还被别的程序使用，统一收在自己的目录下
const PREFIX: &str = "amorimage/";

#[derive(Clone, Debug)]
pub enum Backend {
    Local { root: PathBuf },
    WebDav(WebDavConfig),
    S3(S3Config),
}

#[derive(Clone, Debug)]
pub struct WebDavConfig {
    url: String,
    username: String,
    password: String,
}

#[derive(Clone, Debug)]
pub struct S3Config {
    endpoint: String,
    bucket: String,
    region: String,
    access_key: String,
    secret_key: String,
    /// true 用 `endpoint/bucket/key`，false 用 `bucket.endpoint/key`
    path_style: bool,
}

impl WebDavConfig {
    pub fn new(url: String, username: String, password: String) -> Self {
        WebDavConfig { url, username, password }
    }
}

impl S3Config {
    pub fn new(
        endpoint: String,
        bucket: String,
        region: String,
        access_key: String,
        secret_key: String,
        path_style: bool,
    ) -> Self {
        S3Config {
            endpoint,
            bucket,
            region: if region.trim().is_empty() { "us-east-1".into() } else { region },
            access_key,
            secret_key,
            path_style,
        }
    }
}

/// 按记录里记下的档案 id 取后端；空串或无法解析的老数据用当前档案
pub async fn backend_for(st: &AppState, recorded: &str) -> Result<Backend, AppError> {
    match recorded.parse::<i64>() {
        Ok(id) if id > 0 => match db::storage_profile(&st.pool, id).await? {
            Some(row) => from_profile(&st.cfg.storage_dir, &row),
            None => Err(AppError::Internal(format!(
                "图片记录的存储档案 #{id} 已不存在，无法读取原文件"
            ))),
        },
        _ => backend(st).await,
    }
}

pub async fn backend(st: &AppState) -> Result<Backend, AppError> {
    let id = active_profile_id(st).await?;
    let row = db::storage_profile(&st.pool, id)
        .await?
        .ok_or_else(|| AppError::Internal("当前存储档案不存在".into()))?;
    from_profile(&st.cfg.storage_dir, &row)
}

/// 当前档案 id，存进图片记录里；之后切到别的档案也能按 id 找回原位置
pub async fn active_profile_id(st: &AppState) -> Result<i64, AppError> {
    let raw = setting(st, "storage_profile").await.unwrap_or_default();
    if let Ok(id) = raw.parse::<i64>() {
        return Ok(id);
    }
    // 迁移失败或设置缺失时退到第一个档案，避免整站不可用
    db::storage_profiles(&st.pool)
        .await?
        .first()
        .map(|p| p.id)
        .ok_or_else(|| AppError::Internal("尚未配置任何存储档案".into()))
}

pub fn from_profile(local_root: &std::path::Path, row: &db::ProfileRow) -> Result<Backend, AppError> {
    let cfg: serde_json::Value = serde_json::from_str(&row.config).unwrap_or_default();
    let text = |key: &str| cfg.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    match row.kind.as_str() {
        "webdav" => Ok(Backend::WebDav(WebDavConfig::new(
            text("url"),
            text("username"),
            text("password"),
        ))),
        "s3" => Ok(Backend::S3(S3Config::new(
            text("endpoint"),
            text("bucket"),
            text("region"),
            text("access_key"),
            text("secret_key"),
            cfg.get("path_style").and_then(|v| v.as_bool()).unwrap_or(true),
        ))),
        _ => Ok(Backend::Local { root: local_root.to_path_buf() }),
    }
}

async fn setting(st: &AppState, key: &str) -> Option<String> {
    db::settings_get(&st.pool, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v).ok())
        .map(|v| v.trim().to_string())
}

impl Backend {
    /// 写入一个文件；`sub` 是 "images" 或 "thumbs"，`name` 是不含斜杠的文件名
    pub async fn put(&self, sub: &str, name: &str, data: Bytes) -> Result<(), AppError> {
        match self {
            Backend::Local { root } => {
                tokio::fs::write(root.join(sub).join(name), &data)
                    .await
                    .map_err(|e| AppError::Internal(format!("写入文件失败: {e}")))
            }
            Backend::WebDav(cfg) => cfg.put(&format!("{sub}/{name}"), data).await,
            Backend::S3(cfg) => cfg.put(&format!("{sub}/{name}"), data).await,
        }
    }

    pub async fn get(&self, sub: &str, name: &str) -> Result<Bytes, AppError> {
        match self {
            Backend::Local { root } => tokio::fs::read(root.join(sub).join(name))
                .await
                .map(Bytes::from)
                .map_err(|_| AppError::NotFound("文件不存在".into())),
            Backend::WebDav(cfg) => cfg.get(&format!("{sub}/{name}")).await,
            Backend::S3(cfg) => cfg.get(&format!("{sub}/{name}")).await,
        }
    }

    pub async fn delete(&self, sub: &str, name: &str) {
        if let Err(e) = self.delete_checked(sub, name).await {
            // 删除失败不影响用户请求：最坏只留一个孤儿文件
            tracing::warn!("删除存储对象 {sub}/{name} 失败: {e}");
        }
    }

    /// 与 `delete` 相同但把结果返回给调用方，供清理任务统计失败原因
    pub async fn delete_checked(&self, sub: &str, name: &str) -> Result<(), AppError> {
        let key = format!("{sub}/{name}");
        match self {
            Backend::Local { root } => tokio::fs::remove_file(root.join(sub).join(name))
                .await
                .map_err(|e| AppError::Internal(format!("删除文件失败: {e}"))),
            Backend::WebDav(cfg) => cfg.delete(&key).await,
            Backend::S3(cfg) => cfg.delete(&key).await,
        }
    }

    /// 列出一个子目录（"images" / "thumbs"）下的文件名。
    /// 用于清理孤儿文件：返回的是 printf 用的裸文件名，不含路径
    pub async fn list(&self, sub: &str) -> Result<Vec<String>, AppError> {
        match self {
            Backend::Local { root } => {
                let dir = root.join(sub);
                let mut entries = match tokio::fs::read_dir(&dir).await {
                    Ok(e) => e,
                    // 目录还不存在等于没有文件，不是错误
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                    Err(e) => {
                        return Err(AppError::Internal(format!("读取目录失败: {e}")));
                    }
                };
                let mut names = Vec::new();
                while let Some(entry) = entries
                    .next_entry()
                    .await
                    .map_err(|e| AppError::Internal(format!("遍历目录失败: {e}")))?
                {
                    if entry
                        .file_type()
                        .await
                        .map(|t| t.is_file())
                        .unwrap_or(false)
                    {
                        names.push(entry.file_name().to_string_lossy().into_owned());
                    }
                }
                Ok(names)
            }
            Backend::WebDav(cfg) => cfg.list(sub).await,
            Backend::S3(cfg) => cfg.list(sub).await,
        }
    }

    /// 设置页「测试连接」用：能列出目标位置即视为可用
    pub async fn probe(&self) -> Result<(), String> {
        match self {
            Backend::Local { root } => tokio::fs::create_dir_all(root.join("images"))
                .await
                .map_err(|e| format!("无法写入存储目录: {e}")),
            Backend::WebDav(cfg) => cfg.probe().await,
            Backend::S3(cfg) => cfg.probe().await,
        }
    }
}

fn http_client() -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| AppError::Internal(format!("初始化 HTTP 客户端失败: {e}")))
}

fn http_error(op: &str, _url: &str, e: reqwest::Error) -> AppError {
    // 去掉 reqwest 自带的完整 URL 尾巴：迁移失败清单里每条都带一遍太吵
    let detail = match e.url() {
        Some(u) => e.to_string().replace(&format!(" for url ({u})"), ""),
        None => e.to_string(),
    };
    AppError::Internal(format!("{op}失败: {detail}"))
}

fn status_error(op: &str, status: reqwest::StatusCode, body: &str) -> AppError {
    let detail = body.trim();
    let detail = if detail.is_empty() || detail.len() > 200 { "" } else { detail };
    AppError::Internal(format!("{op}失败（HTTP {}）{detail}", status.as_u16()))
}

/// 从 WebDAV 的 multistatus 响应里取文件名。
///
/// 只认 `<href>…</href>` 的内容，取最后一段路径并做百分号解码。目录自身那条
/// 以 `/` 结尾会被排除。返回的名字都是裸文件名，调用方仍须对照白名单使用。
/// 不用完整 XML 解析器：多状态响应的命名空间写法各家不一，靠文本提取 href
/// 更省事，且 URL 规范化会吃掉 NS 前缀差异。
fn collect_hrefs(body: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = body;
    while let Some(start) = find_ci(rest, "<d:href>").or_else(|| find_ci(rest, "<href>")) {
        let after_tag = &rest[start..];
        let open_len = if after_tag[..8].eq_ignore_ascii_case("<d:href>") { 8 } else { 6 };
        let content_start = start + open_len;
        let tail = &rest[content_start..];
        let Some(end) = find_ci(tail, "</d:href>").or_else(|| find_ci(tail, "</href>")) else {
            break;
        };
        let raw = tail[..end].trim();
        rest = &tail[end..];
        let decoded = percent_decode(raw);
        // 目录条目以 / 结尾，跳过；再取最后一段路径
        if decoded.ends_with('/') {
            continue;
        }
        let Some(last) = decoded.rsplit('/').next() else {
            continue;
        };
        let last = last.trim();
        if !last.is_empty() {
            names.push(last.to_string());
        }
    }
    names
}

/// 最小百分号解码：只处理 %XX，非法/不完整序列原样保留。
/// 文件名里只可能出现少数需要转义的字符，够用了，省得为它引入依赖。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                b'A'..=b'F' => Some(b - b'A' + 10),
                _ => None,
            };
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || n.len() > h.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

async fn check_put(resp: reqwest::Response, url: &str) -> Result<(), AppError> {
    if resp.status().is_success() {
        return Ok(());
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Err(status_error("上传到 WebDAV ", status, &body))
        .map_err(|e| match e {
            AppError::Internal(_) => AppError::Internal(format!("{e}（{url}）")),
            e => e,
        })
}

impl WebDavConfig {
    /// 后缀按目录拼到配置的根地址上，末尾斜杠可有可无
    fn url_for(&self, key: &str) -> String {
        format!("{}/{}", self.url.trim_end_matches('/'), key)
    }

    async fn probe(&self) -> Result<(), String> {
        if self.url.is_empty() {
            return Err("WebDAV 地址为空".into());
        }
        let url = self.url_for("probe");
        let client = http_client().map_err(|e| e.to_string())?;
        let mut req = client
            .request(reqwest::Method::from_bytes(b"PROPFIND").expect("静态方法"), &url)
            .header("Depth", "0");
        if !self.username.is_empty() {
            req = req.basic_auth(&self.username, Some(&self.password));
        }
        let resp = req.send().await.map_err(|e| format!("连接失败: {e}"))?;
        let status = resp.status();
        // 401/403 说明连通但凭据不对；404 说明目录不存在（WebDAV 服务器同样能报出这点）
        match status.as_u16() {
            200..=299 | 404 | 405 => Ok(()),
            401 | 403 => Err("连接成功，但用户名或密码被拒绝".into()),
            _ => Err(format!("服务器返回 HTTP {}", status.as_u16())),
        }
    }

    async fn put(&self, key: &str, data: Bytes) -> Result<(), AppError> {
        let client = http_client()?;
        let url = self.url_for(key);
        let resp = self.put_once(&client, &url, data.clone()).await?;
        // 409 表示父目录还不存在：逐级建目录后再试一次。平常用的是正常的 PUT，
        // 只有首次上传到新目录才会多走这一趟
        if resp.status() == reqwest::StatusCode::CONFLICT {
            self.ensure_dirs(&client, key).await?;
            let resp = self.put_once(&client, &url, data).await?;
            return check_put(resp, &url).await;
        }
        check_put(resp, &url).await
    }

    async fn put_once(
        &self,
        client: &reqwest::Client,
        url: &str,
        data: Bytes,
    ) -> Result<reqwest::Response, AppError> {
        let mut req = client.put(url).body(data);
        if !self.username.is_empty() {
            req = req.basic_auth(&self.username, Some(&self.password));
        }
        req.send().await.map_err(|e| http_error("上传到 WebDAV", url, e))
    }

    async fn ensure_dirs(&self, client: &reqwest::Client, key: &str) -> Result<(), AppError> {
        let Some((dir, _)) = key.rsplit_once('/') else {
            return Ok(());
        };
        let mut base = self.url.trim_end_matches('/').to_string();
        for segment in dir.split('/').filter(|s| !s.is_empty()) {
            base.push('/');
            base.push_str(segment);
            let method = reqwest::Method::from_bytes(b"MKCOL").expect("静态方法");
            let mut req = client.request(method, &base);
            if !self.username.is_empty() {
                req = req.basic_auth(&self.username, Some(&self.password));
            }
            let resp = req
                .send()
                .await
                .map_err(|e| http_error("创建 WebDAV 目录", &base, e))?;
            let status = resp.status();
            // 405 表示目录已存在，属正常情况
            if !status.is_success() && status.as_u16() != 405 {
                if status == reqwest::StatusCode::CONFLICT {
                    return Err(AppError::Internal(format!(
                        "WebDAV 根目录 {base} 不存在，请先在服务器上创建"
                    )));
                }
                return Err(AppError::Internal(format!(
                    "创建 WebDAV 目录失败（HTTP {}）",
                    status.as_u16()
                )));
            }
        }
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Bytes, AppError> {
        let url = self.url_for(key);
        let client = http_client()?;
        let mut req = client.get(&url);
        if !self.username.is_empty() {
            req = req.basic_auth(&self.username, Some(&self.password));
        }
        let resp = req.send().await.map_err(|e| http_error("从 WebDAV 读取", &url, e))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AppError::NotFound("文件不存在".into()));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(AppError::Internal(format!(
                "从 WebDAV 读取失败（HTTP {}）",
                status.as_u16()
            )));
        }
        resp.bytes().await.map_err(|e| http_error("读取 WebDAV 响应", &url, e))
    }

    /// Depth:1 PROPFIND 列出子目录里的文件。只取响应里的 <d:href>，
    /// 用最后一段路径当文件名，避免依赖各服务器不一的 XML 结构
    async fn list(&self, sub: &str) -> Result<Vec<String>, AppError> {
        let url = format!("{}/", self.url_for(sub).trim_end_matches('/'));
        let client = http_client()?;
        let mut req = client
            .request(reqwest::Method::from_bytes(b"PROPFIND").expect("静态方法"), &url)
            .header("Depth", "1");
        if !self.username.is_empty() {
            req = req.basic_auth(&self.username, Some(&self.password));
        }
        let resp = req.send().await.map_err(|e| http_error("列出 WebDAV 目录", &url, e))?;
        // 目录还没建过：没有文件，不是错误
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::MULTI_STATUS {
            let status = resp.status();
            return Err(AppError::Internal(format!(
                "列出 WebDAV 目录失败（HTTP {}）",
                status.as_u16()
            )));
        }
        let body = resp.text().await.map_err(|e| http_error("读取 WebDAV 响应", &url, e))?;
        Ok(collect_hrefs(&body))
    }

    async fn delete(&self, key: &str) -> Result<(), AppError> {
        let url = self.url_for(key);
        let client = http_client()?;
        let mut req = client.delete(&url);
        if !self.username.is_empty() {
            req = req.basic_auth(&self.username, Some(&self.password));
        }
        let resp = req.send().await.map_err(|e| http_error("删除 WebDAV 文件", &url, e))?;
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
            let status = resp.status();
            return Err(AppError::Internal(format!(
                "删除 WebDAV 文件失败（HTTP {}）",
                status.as_u16()
            )));
        }
        Ok(())
    }
}

impl S3Config {
    fn bucket(&self) -> Result<(Bucket, Credentials), AppError> {
        let bad = |msg: &str| AppError::Internal(format!("S3 配置有误: {msg}"));
        if self.endpoint.is_empty() || self.bucket.is_empty() {
            return Err(bad("endpoint 与 bucket 不能为空"));
        }
        let endpoint = self
            .endpoint
            .parse::<url::Url>()
            .map_err(|e| bad(&format!("endpoint 不是合法 URL: {e}")))?;
        let style = if self.path_style { UrlStyle::Path } else { UrlStyle::VirtualHost };
        let bucket = Bucket::new(endpoint, style, self.bucket.clone(), self.region.clone())
            .map_err(|e| bad(&format!("构建 bucket 失败: {e}")))?;
        let creds = Credentials::new(self.access_key.clone(), self.secret_key.clone());
        Ok((bucket, creds))
    }

    async fn probe(&self) -> Result<(), String> {
        let (bucket, creds) = self.bucket().map_err(|e| e.to_string())?;
        let action = bucket.head_bucket(Some(&creds));
        let url = action.sign(Duration::from_secs(300));
        let client = http_client().map_err(|e| e.to_string())?;
        let resp = client.head(url.as_str()).send().await.map_err(|e| format!("连接失败: {e}"))?;
        match resp.status().as_u16() {
            200..=299 => Ok(()),
            403 => Err("连接成功，但 Access Key 被拒绝".into()),
            404 => Err("连接成功，但存储桶不存在".into()),
            code => Err(format!("服务器返回 HTTP {code}")),
        }
    }

    async fn put(&self, key: &str, data: Bytes) -> Result<(), AppError> {
        let (bucket, creds) = self.bucket()?;
        let key = format!("{PREFIX}{key}");
        let mut action = bucket.put_object(Some(&creds), &key);
        action.headers_mut().insert("content-length", data.len().to_string());
        let url = action.sign(Duration::from_secs(300));
        let client = http_client()?;
        let resp = client
            .put(url.as_str())
            .body(data)
            .send()
            .await
            .map_err(|e| http_error("上传到 S3", url.as_str(), e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(status_error("上传到 S3 ", status, &body));
        }
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Bytes, AppError> {
        let (bucket, creds) = self.bucket()?;
        let key = format!("{PREFIX}{key}");
        let url = bucket.get_object(Some(&creds), &key).sign(Duration::from_secs(300));
        let client = http_client()?;
        let resp = client
            .get(url.as_str())
            .send()
            .await
            .map_err(|e| http_error("从 S3 读取", url.as_str(), e))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AppError::NotFound("文件不存在".into()));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(AppError::Internal(format!(
                "从 S3 读取失败（HTTP {}）",
                status.as_u16()
            )));
        }
        resp.bytes().await.map_err(|e| http_error("读取 S3 响应", url.as_str(), e))
    }

    /// ListObjectsV2 分页列举某个子目录下的对象，返回裸文件名
    async fn list(&self, sub: &str) -> Result<Vec<String>, AppError> {
        let (bucket, creds) = self.bucket()?;
        let prefix = format!("{PREFIX}{sub}/");
        let client = http_client()?;
        let mut names = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut action = bucket.list_objects_v2(Some(&creds));
            action.with_prefix(&prefix);
            action.with_max_keys(1000);
            if let Some(t) = token.as_deref() {
                action.with_continuation_token(t);
            }
            let url = action.sign(Duration::from_secs(300));
            let resp = client
                .get(url.as_str())
                .send()
                .await
                .map_err(|e| http_error("列出 S3 对象", url.as_str(), e))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(status_error("列出 S3 对象 ", status, &body));
            }
            let body = resp.text().await.map_err(|e| http_error("读取 S3 响应", url.as_str(), e))?;
            // parse_response 负责 URL 解码与截断校验，直接给最终结构
            let page = rusty_s3::actions::ListObjectsV2::parse_response(&body)
                .map_err(|e| AppError::Internal(format!("解析 S3 列表响应失败: {e}")))?;
            for item in &page.contents {
                let Some(rest) = item.key.strip_prefix(&prefix) else { continue };
                if !rest.is_empty() && !rest.contains('/') {
                    names.push(rest.to_string());
                }
            }
            match page.next_continuation_token {
                Some(t) if !t.is_empty() => token = Some(t),
                _ => break,
            }
        }
        Ok(names)
    }

    async fn delete(&self, key: &str) -> Result<(), AppError> {
        let (bucket, creds) = self.bucket()?;
        let key = format!("{PREFIX}{key}");
        let url = bucket.delete_object(Some(&creds), &key).sign(Duration::from_secs(300));
        let client = http_client()?;
        let resp = client
            .delete(url.as_str())
            .send()
            .await
            .map_err(|e| http_error("删除 S3 对象", url.as_str(), e))?;
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
            let status = resp.status();
            return Err(AppError::Internal(format!(
                "删除 S3 对象失败（HTTP {}）",
                status.as_u16()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn webdav(url: &str) -> WebDavConfig {
        WebDavConfig::new(url.into(), "u".into(), "p".into())
    }

    #[test]
    fn webdav_url_joins_single_slash() {
        assert_eq!(
            webdav("https://dav.example.com/root/").url_for("images/a.png"),
            "https://dav.example.com/root/images/a.png"
        );
        assert_eq!(
            webdav("https://dav.example.com/root").url_for("thumbs/a.webp"),
            "https://dav.example.com/root/thumbs/a.webp"
        );
    }

    #[test]
    fn s3_region_defaults_when_blank() {
        let cfg = S3Config::new(
            "https://s3.example.com".into(),
            "bucket".into(),
            "  ".into(),
            "k".into(),
            "s".into(),
            true,
        );
        assert_eq!(cfg.region, "us-east-1");
    }

    #[test]
    fn s3_rejects_missing_endpoint() {
        let cfg = S3Config::new(String::new(), "bucket".into(), String::new(), String::new(), String::new(), true);
        assert!(cfg.bucket().is_err());
    }

    #[test]
    fn s3_builds_bucket_for_both_url_styles() {
        for path_style in [true, false] {
            let cfg = S3Config::new(
                "https://s3.example.com".into(),
                "my-bucket".into(),
                "us-east-1".into(),
                "k".into(),
                "s".into(),
                path_style,
            );
            assert!(cfg.bucket().is_ok(), "path_style={path_style}");
        }
    }

    #[test]
    fn collect_hrefs_extracts_filenames_only() {
        let body = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
  <d:response><d:href>/amorimage/images/</d:href>
    <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
  </d:response>
  <d:response><d:href>/amorimage/images/abc123.png</d:href></d:response>
  <d:response><d:href>/amorimage/images/def456.webp</d:href></d:response>
</d:multistatus>"#;
        let names = collect_hrefs(body);
        assert_eq!(names, vec!["abc123.png", "def456.webp"]);
    }

    #[test]
    fn collect_hrefs_decodes_percent_escapes() {
        let body = "<multistatus><response><href>/dav/thumbs/a%20b.webp</href></response></multistatus>";
        assert_eq!(collect_hrefs(body), vec!["a b.webp"]);
    }

    #[test]
    fn collect_hrefs_without_namespace_prefix() {
        let body = "<multistatus><response><href>/x/images/one.png</href></response></multistatus>";
        assert_eq!(collect_hrefs(body), vec!["one.png"]);
    }

    #[test]
    fn percent_decode_leaves_bad_sequences_alone() {
        assert_eq!(percent_decode("a%zzb"), "a%zzb");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%E4%BD%A0.png"), "你.png");
    }

    #[test]
    fn profile_config_roundtrips_through_from_profile() {
        let root = std::env::temp_dir();
        let row = db::ProfileRow {
            id: 7,
            name: "S3".into(),
            kind: "s3".into(),
            config: r#"{"endpoint":"https://s3.example.com","bucket":"b","region":"eu-west-1",
                        "access_key":"k","secret_key":"s","path_style":false}"#
                .to_string(),
        };
        match from_profile(&root, &row).unwrap() {
            Backend::S3(cfg) => {
                assert_eq!(cfg.endpoint, "https://s3.example.com");
                assert_eq!(cfg.region, "eu-west-1");
                assert!(!cfg.path_style);
            }
            other => panic!("期望 S3，得到 {other:?}"),
        }
    }

    #[test]
    fn profile_with_broken_json_still_yields_local() {
        let root = std::env::temp_dir();
        let row = db::ProfileRow {
            id: 1,
            name: "本地".into(),
            kind: "local".into(),
            config: "not json".into(),
        };
        assert!(matches!(from_profile(&root, &row).unwrap(), Backend::Local { .. }));
    }
}
