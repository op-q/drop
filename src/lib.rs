pub mod app_state;
pub mod config;
pub mod domain;
pub mod errors;
pub mod routes;
pub mod services;
pub mod store;
pub mod telemetry;
pub mod ws;

use std::net::SocketAddr;

use app_state::AppState;
use axum::{
    Router,
    routing::{get, get_service, post},
};
use config::cors_layer_from_env;
use routes::{
    download_ws::download_ws, metrics::metrics, sessions::create_session, upload_ws::upload_ws,
};
use services::{cleanup_service::spawn_cleanup_task, rate_limit_service::RateLimitService};
use store::InMemorySessionStore;
use telemetry::{metrics::AppMetrics, metrics::spawn_metrics_task};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::{DefaultOnFailure, DefaultOnRequest, DefaultOnResponse, TraceLayer},
};
use tracing::Level;

pub fn build_state() -> AppState {
    AppState {
        sessions: InMemorySessionStore::new(),
        rate_limiter: RateLimitService::new(),
        metrics: AppMetrics::new(),
    }
}

pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route_service("/", get_service(ServeFile::new("web/dist/index.html")))
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/api/session/create", post(create_session))
        .route("/ws/upload/:code", get(upload_ws))
        .route("/ws/download/:code", get(download_ws))
        .nest_service("/assets", ServeDir::new("web/dist/assets"))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(telemetry::tracing::make_http_span)
                .on_request(DefaultOnRequest::new().level(Level::DEBUG))
                .on_response(DefaultOnResponse::new().level(Level::INFO))
                .on_failure(DefaultOnFailure::new().level(Level::ERROR)),
        )
        .layer(cors_layer_from_env())
        .with_state(state)
}

pub fn start_background_services(state: AppState) {
    spawn_cleanup_task(state.clone());
    spawn_metrics_task(state);
}

pub async fn serve(listener: tokio::net::TcpListener, app: Router) -> std::io::Result<()> {
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

async fn health() -> &'static str {
    "ok"
}
