mod assets;
mod auth;
mod cleanup;
mod config;
mod db;
mod error;
mod imaging;
mod migrate;
mod routes;
mod settings;
mod state;
mod store;

use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};
use axum::Router;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::state::AppState;

#[tokio::main]
async fn main() {
    let cfg = config::Config::from_env();
    init_tracing();

    std::fs::create_dir_all(&cfg.db_dir).expect("创建数据库目录失败");
    std::fs::create_dir_all(cfg.storage_dir.join("images")).expect("创建图片目录失败");
    std::fs::create_dir_all(cfg.storage_dir.join("thumbs")).expect("创建缩略图目录失败");

    let bind = cfg.bind;
    let db_dir = cfg.db_dir.clone();
    let storage_dir = cfg.storage_dir.clone();
    let has_token = cfg.token.is_some();
    let state = AppState {
        pool: db::init(&cfg).await.expect("初始化数据库失败"),
        cfg: std::sync::Arc::new(cfg),
        migrate: std::sync::Arc::new(migrate::MigrateState::new()),
        cleanup: std::sync::Arc::new(cleanup::CleanupState::new()),
    };

    // 上传路由关闭 axum 的默认 2MB 传输上限，改由处理器按设置流式检查；
    // favicon 上传路由单独给 16MB 传输上限兜底；其余路由保持默认防护
    let app = Router::new()
        .route(
            "/api/upload",
            post(routes::upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/images", get(routes::list_images))
        .route("/api/images/{id}", delete(routes::delete_image))
        .route("/api/login", post(auth::login))
        .route("/api/logout", post(auth::logout))
        .route(
            "/api/settings",
            get(settings::get).put(settings::update),
        )
        .route(
            "/api/settings/storage/profiles",
            post(settings::save_profile).delete(settings::delete_profile),
        )
        .route("/api/settings/storage/probe", post(settings::probe_storage))
        .route("/api/settings/storage/migrate", post(settings::start_migration))
        .route("/api/settings/storage/migrate", delete(settings::cancel_migration))
        .route("/api/settings/storage/migrate/progress", get(settings::migration_progress))
        .route("/api/settings/storage/orphans", post(settings::scan_orphans))
        .route("/api/settings/storage/orphans", delete(settings::delete_orphans))
        .route(
            "/api/settings/storage/orphans/progress",
            get(settings::cleanup_progress),
        )
        .route(
            "/api/settings/storage/orphans/cancel",
            post(settings::cancel_cleanup),
        )
        .route(
            "/api/settings/favicon",
            put(settings::upload_favicon)
                .delete(settings::delete_favicon)
                .layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route("/favicon.ico", get(settings::serve_favicon))
        .route("/img/{fname}", get(routes::serve_image))
        .route("/thumb/{fname}", get(routes::serve_thumb))
        .route("/healthz", get(|| async { "ok" }))
        .fallback(assets::serve_static)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::gate,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .unwrap_or_else(|e| panic!("监听 {bind} 失败: {e}"));
    tracing::info!("amorimage 已启动: http://{bind}");
    tracing::info!("数据库目录: {}", db_dir.display());
    tracing::info!("图片目录: {}", storage_dir.display());
    tracing::info!(
        "鉴权: {}",
        if has_token { "已启用，未登录将只能查看图片外链" } else { "未设置令牌，开放模式" }
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("服务异常退出");
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("amorimage=debug,tower_http=warn"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

async fn shutdown_signal() {
    // 容器里进程是 PID 1，内核不提供默认信号处理，必须显式监听 SIGTERM，
    // 否则 `docker compose stop` 等满宽限期后被 SIGKILL，优雅停机形同虚设
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("安装 SIGTERM 信号处理器失败");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("安装 Ctrl+C 信号处理器失败");
    }
}
