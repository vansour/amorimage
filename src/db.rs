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
            created_at  INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value BLOB NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    Ok(pool)
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
            (hash, orig_name, ext, mime, width, height, size, thumb_size, created_at)
         VALUES (?,?,?,?,?,?,?,?,?)",
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
