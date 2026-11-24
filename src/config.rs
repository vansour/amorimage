use std::{net::SocketAddr, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub db_dir: PathBuf,
    pub storage_dir: PathBuf,
    pub base_url: Option<String>,
    pub token: Option<String>,
}

impl Config {
    pub fn from_env() -> Config {
        let bind = env_str("AMOR_BIND")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| "127.0.0.1:3000".parse().expect("valid socket addr"));
        let token = env_str("AMOR_TOKEN");
        if let Some(t) = &token {
            // 含非 ASCII 字节会让 Set-Cookie 构造失败（登录永远 500），
            // 含分号则浏览器会把 Cookie 截断、永远对不上令牌，启动即拦截
            if t.len() > 128
                || t.bytes().any(|b| !(0x21..=0x7e).contains(&b) || b == b';')
            {
                panic!("AMOR_TOKEN 非法：仅支持不超过 128 个字符的可见 ASCII，且不能包含分号");
            }
        }
        Config {
            bind,
            db_dir: PathBuf::from(env_str("AMOR_DB_DIR").unwrap_or_else(|| "data/db".into())),
            storage_dir: PathBuf::from(
                env_str("AMOR_STORAGE_DIR").unwrap_or_else(|| "data/storage".into()),
            ),
            // 不设置时由前端用浏览器地址自动拼接，此变量仅作强制覆盖
            base_url: env_str("AMOR_BASE_URL").map(|v| v.trim_end_matches('/').to_string()),
            token,
        }
    }
}

fn env_str(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}
