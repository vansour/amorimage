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

/// 按记录里记下的后端名取后端；旧数据（空串）用当前设置
pub async fn backend_for(st: &AppState, recorded: &str) -> Result<Backend, AppError> {
    if recorded.is_empty() {
        return backend(st).await;
    }
    build(st, Some(recorded)).await
}

pub async fn backend(st: &AppState) -> Result<Backend, AppError> {
    build(st, setting(st, "storage_backend").await.as_deref()).await
}

/// 当前后端的名字，存进图片记录里，用于切换后仍能从原位置读回旧图
pub async fn backend_name(st: &AppState) -> String {
    setting(st, "storage_backend")
        .await
        .unwrap_or_else(|| "local".into())
}

async fn build(st: &AppState, kind: Option<&str>) -> Result<Backend, AppError> {
    match kind {
        Some("webdav") => Ok(Backend::WebDav(WebDavConfig {
            url: setting(st, "webdav_url").await.unwrap_or_default(),
            username: setting(st, "webdav_username").await.unwrap_or_default(),
            password: setting(st, "webdav_password").await.unwrap_or_default(),
        })),
        Some("s3") => Ok(Backend::S3(S3Config {
            endpoint: setting(st, "s3_endpoint").await.unwrap_or_default(),
            bucket: setting(st, "s3_bucket").await.unwrap_or_default(),
            region: setting(st, "s3_region")
                .await
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "us-east-1".into()),
            access_key: setting(st, "s3_access_key").await.unwrap_or_default(),
            secret_key: setting(st, "s3_secret_key").await.unwrap_or_default(),
            path_style: setting(st, "s3_path_style").await.as_deref() != Some("0"),
        })),
        _ => Ok(Backend::Local { root: st.cfg.storage_dir.clone() }),
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
        let key = format!("{sub}/{name}");
        let result = match self {
            Backend::Local { root } => tokio::fs::remove_file(root.join(sub).join(name))
                .await
                .map_err(|e| e.to_string()),
            Backend::WebDav(cfg) => cfg.delete(&key).await.map_err(|e| e.to_string()),
            Backend::S3(cfg) => cfg.delete(&key).await.map_err(|e| e.to_string()),
        };
        // 删除失败不影响用户请求：最坏只留一个孤儿文件
        if let Err(e) = result {
            tracing::warn!("删除存储对象 {key} 失败: {e}");
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

fn http_error(op: &str, url: &str, e: reqwest::Error) -> AppError {
    AppError::Internal(format!("{op} {url} 失败: {e}"))
}

fn status_error(op: &str, status: reqwest::StatusCode, body: &str) -> AppError {
    let detail = body.trim();
    let detail = if detail.is_empty() || detail.len() > 200 { "" } else { detail };
    AppError::Internal(format!("{op}失败（HTTP {}）{detail}", status.as_u16()))
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
}
