use std::sync::Arc;
use sqlx::SqlitePool;
use crate::cleanup::CleanupState;
use crate::config::Config;
use crate::migrate::MigrateState;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub cfg: Arc<Config>,
    pub migrate: Arc<MigrateState>,
    pub cleanup: Arc<CleanupState>,
}
