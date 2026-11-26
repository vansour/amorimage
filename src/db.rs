use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::config::Config;

#[derive(sqlx::FromRow, serde::Serialize)]
pub struct ImageRow {
    pub id: i64,
    pub hash: String,
    pub orig_name: String,
    pub ext: String,
    pub mime: String,
    pub width: i64,
    pub height: i64,
    pub size: i64,
    pub thumb_size: i64,
    pub created_at: i64,
    /// 上传时所用的存储档案 id；旧数据可能是 local/webdav/s3 字样
    #[serde(skip)]
    pub backend: String,
}

/// 一条命名的存储配置。切换后端不会改旧档案，旧图仍能按档案找回原位置
#[derive(sqlx::FromRow, Clone, Debug)]
pub struct ProfileRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub config: String,
}

pub struct NewImage<'a> {
    pub hash: &'a str,
    pub orig_name: &'a str,
    pub ext: &'a str,
    pub mime: &'a str,
    pub width: u32,
    pub height: u32,
    pub size: u64,
    pub thumb_size: u64,
    pub created_at: i64,
    /// 落库时的存储后端标识，之后切后端也能按记录找回文件
    pub backend: &'a str,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub async fn init(cfg: &Config) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::new()
        .filename(cfg.db_dir.join("amorimage.db"))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS images (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            hash        TEXT NOT NULL UNIQUE,
            orig_name   TEXT NOT NULL,
            ext         TEXT NOT NULL,
            mime        TEXT NOT NULL,
            width       INTEGER NOT NULL,
            height      INTEGER NOT NULL,
            size        INTEGER NOT NULL,
            thumb_size  INTEGER NOT NULL,
            created_at  INTEGER NOT NULL,
            backend     TEXT NOT NULL DEFAULT ''
        )",
    )
    .execute(&pool)
    .await?;
    // 早期版本的库没有 backend 列：补上，旧记录留空串表示「跟随当前设置」
    let has_backend = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pragma_table_info('images') WHERE name = 'backend'",
    )
    .fetch_one(&pool)
    .await?;
    if has_backend == 0 {
        sqlx::query("ALTER TABLE images ADD COLUMN backend TEXT NOT NULL DEFAULT ''")
            .execute(&pool)
            .await?;
    }
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value BLOB NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS storage_profiles (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            name       TEXT NOT NULL,
            kind       TEXT NOT NULL,
            config     TEXT NOT NULL DEFAULT '{}',
            created_at INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    migrate_storage_profiles(&pool).await?;
    Ok(pool)
}

/// 老版本把存储配置平铺在 settings 里、图片记录存后端类型名。这里一次性
/// 转成 storage_profiles 档案表 + 图片记录存档案 id。用哨兵键保证只跑一次，
/// 中途崩溃重跑最多多出重复档案，不会丢配置。
async fn migrate_storage_profiles(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    if settings_get(pool, "storage_profiles_ready").await?.is_none() {
        let active_kind = text_setting(pool, "storage_backend").await;
        let local_id = storage_profile_insert(pool, "本地磁盘", "local", "{}").await?;

        let webdav_url = text_setting(pool, "webdav_url").await;
        let webdav_id = if !webdav_url.is_empty() || count_by_backend(pool, "webdav").await? > 0 {
            let cfg = serde_json::json!({
                "url": webdav_url,
                "username": text_setting(pool, "webdav_username").await,
                "password": text_setting(pool, "webdav_password").await,
            });
            let id = storage_profile_insert(pool, "WebDAV", "webdav", &cfg.to_string()).await?;
            sqlx::query("UPDATE images SET backend = ? WHERE backend = 'webdav'")
                .bind(id.to_string())
                .execute(pool)
                .await?;
            Some(id)
        } else {
            None
        };

        let s3_endpoint = text_setting(pool, "s3_endpoint").await;
        let s3_id = if !s3_endpoint.is_empty() || count_by_backend(pool, "s3").await? > 0 {
            let cfg = serde_json::json!({
                "endpoint": s3_endpoint,
                "bucket": text_setting(pool, "s3_bucket").await,
                "region": text_setting(pool, "s3_region").await,
                "access_key": text_setting(pool, "s3_access_key").await,
                "secret_key": text_setting(pool, "s3_secret_key").await,
                "path_style": text_setting(pool, "s3_path_style").await != "0",
            });
            let id = storage_profile_insert(pool, "S3", "s3", &cfg.to_string()).await?;
            sqlx::query("UPDATE images SET backend = ? WHERE backend = 's3'")
                .bind(id.to_string())
                .execute(pool)
                .await?;
            Some(id)
        } else {
            None
        };

        let active_id = match active_kind.as_str() {
            "webdav" => webdav_id.unwrap_or(local_id),
            "s3" => s3_id.unwrap_or(local_id),
            _ => local_id,
        };
        // 空串是「跟随当前设置」的老语义，落到当时的活动档案上
        sqlx::query("UPDATE images SET backend = ? WHERE backend = ''")
            .bind(active_id.to_string())
            .execute(pool)
            .await?;
        sqlx::query("UPDATE images SET backend = ? WHERE backend = 'local'")
            .bind(local_id.to_string())
            .execute(pool)
            .await?;
        settings_set(pool, "storage_profile", active_id.to_string().as_bytes()).await?;
        settings_set(pool, "storage_profiles_ready", b"1").await?;
        for key in [
            "storage_backend",
            "webdav_url",
            "webdav_username",
            "webdav_password",
            "s3_endpoint",
            "s3_bucket",
            "s3_region",
            "s3_access_key",
            "s3_secret_key",
            "s3_path_style",
        ] {
            settings_delete(pool, key).await?;
        }
    }

    // 指针必须指向真实存在的档案，否则整站读不到任何图片
    let current = text_setting(pool, "storage_profile").await;
    let profiles = storage_profiles(pool).await?;
    let valid = current
        .parse::<i64>()
        .ok()
        .is_some_and(|id| profiles.iter().any(|p| p.id == id));
    if !valid && let Some(fallback) = profiles.first() {
        settings_set(pool, "storage_profile", fallback.id.to_string().as_bytes()).await?;
    }
    Ok(())
}

async fn text_setting(pool: &SqlitePool, key: &str) -> String {
    settings_get(pool, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v).ok())
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

pub async fn settings_get(
    pool: &SqlitePool,
    key: &str,
) -> Result<Option<Vec<u8>>, sqlx::Error> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
}

pub async fn settings_set(pool: &SqlitePool, key: &str, value: &[u8]) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn settings_delete(pool: &SqlitePool, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn count(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM images")
        .fetch_one(pool)
        .await
}

pub async fn list(
    pool: &SqlitePool,
    limit: i64,
    before: Option<i64>,
) -> Result<Vec<ImageRow>, sqlx::Error> {
    match before {
        Some(before) => {
            sqlx::query_as::<_, ImageRow>(
                "SELECT * FROM images WHERE id < ? ORDER BY id DESC LIMIT ?",
            )
            .bind(before)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, ImageRow>("SELECT * FROM images ORDER BY id DESC LIMIT ?")
                .bind(limit)
                .fetch_all(pool)
                .await
        }
    }
}

pub async fn find_by_hash(pool: &SqlitePool, hash: &str) -> Result<Option<ImageRow>, sqlx::Error> {
    sqlx::query_as::<_, ImageRow>("SELECT * FROM images WHERE hash = ?")
        .bind(hash)
        .fetch_optional(pool)
        .await
}

pub async fn find_by_id(pool: &SqlitePool, id: i64) -> Result<Option<ImageRow>, sqlx::Error> {
    sqlx::query_as::<_, ImageRow>("SELECT * FROM images WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

pub async fn insert(pool: &SqlitePool, img: &NewImage<'_>) -> Result<i64, sqlx::Error> {
    let res = sqlx::query(
        "INSERT INTO images
            (hash, orig_name, ext, mime, width, height, size, thumb_size, created_at, backend)
         VALUES (?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(img.hash)
    .bind(img.orig_name)
    .bind(img.ext)
    .bind(img.mime)
    .bind(img.width as i64)
    .bind(img.height as i64)
    .bind(img.size as i64)
    .bind(img.thumb_size as i64)
    .bind(img.created_at)
    .bind(img.backend)
    .execute(pool)
    .await?;
    Ok(res.last_insert_rowid())
}

pub async fn delete(pool: &SqlitePool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM images WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------- 存储档案 ----------

pub async fn storage_profiles(pool: &SqlitePool) -> Result<Vec<ProfileRow>, sqlx::Error> {
    sqlx::query_as::<_, ProfileRow>(
        "SELECT id, name, kind, config FROM storage_profiles ORDER BY id",
    )
    .fetch_all(pool)
    .await
}

pub async fn storage_profile(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<ProfileRow>, sqlx::Error> {
    sqlx::query_as::<_, ProfileRow>(
        "SELECT id, name, kind, config FROM storage_profiles WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn storage_profile_insert(
    pool: &SqlitePool,
    name: &str,
    kind: &str,
    config: &str,
) -> Result<i64, sqlx::Error> {
    let res = sqlx::query(
        "INSERT INTO storage_profiles (name, kind, config, created_at) VALUES (?,?,?,?)",
    )
    .bind(name)
    .bind(kind)
    .bind(config)
    .bind(now_ms())
    .execute(pool)
    .await?;
    Ok(res.last_insert_rowid())
}

pub async fn storage_profile_update(
    pool: &SqlitePool,
    id: i64,
    name: &str,
    config: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE storage_profiles SET name = ?, config = ? WHERE id = ?")
        .bind(name)
        .bind(config)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn storage_profile_delete(pool: &SqlitePool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM storage_profiles WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn count_by_backend(pool: &SqlitePool, backend: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM images WHERE backend = ?")
        .bind(backend)
        .fetch_one(pool)
        .await
}

/// 待迁移：backend 不等于目标档案的全部记录（按 id 升序分批取）
pub async fn list_pending_move(
    pool: &SqlitePool,
    target: i64,
    after_id: i64,
    limit: i64,
) -> Result<Vec<ImageRow>, sqlx::Error> {
    sqlx::query_as::<_, ImageRow>(
        "SELECT * FROM images WHERE backend <> ? AND id > ? ORDER BY id LIMIT ?",
    )
    .bind(target.to_string())
    .bind(after_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub async fn count_pending_move(pool: &SqlitePool, target: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM images WHERE backend <> ?")
        .bind(target.to_string())
        .fetch_one(pool)
        .await
}

/// 该 hash 是否仍被给定档案集合中的任一记录引用
pub async fn hash_owned_by(
    pool: &SqlitePool,
    hash: &str,
    backends: &[i64],
) -> Result<bool, sqlx::Error> {
    if backends.is_empty() {
        return Ok(false);
    }
    let mut qb = sqlx::QueryBuilder::new("SELECT COUNT(*) FROM images WHERE hash = ");
    qb.push_bind(hash).push(" AND backend IN (");
    let mut list = qb.separated(", ");
    for id in backends {
        list.push_bind(id.to_string());
    }
    list.push_unseparated(")");
    Ok(qb.build_query_scalar::<i64>().fetch_one(pool).await? > 0)
}

/// 某档案下所有记录的 hash：判定「该档案上的文件是否还有归属」
pub async fn hashes_by_backend(
    pool: &SqlitePool,
    backend: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT hash FROM images WHERE backend = ?")
        .bind(backend)
        .fetch_all(pool)
        .await
}

pub async fn set_backend(pool: &SqlitePool, id: i64, backend: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE images SET backend = ? WHERE id = ?")
        .bind(backend)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
