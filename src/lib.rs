pub mod app_state;
pub mod config;
pub mod domain;
pub mod errors;
pub mod routes;
pub mod services;
pub mod store;
pub mod telemetry;
pub mod ws;

use std::{future::Future, net::SocketAddr};

use app_state::AppState;
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use routes::{
    download_ws::download_ws, metrics::metrics, sessions::create_session, upload_ws::upload_ws,
};
use services::{cleanup_service::spawn_cleanup_task, rate_limit_service::RateLimitService};
use store::InMemorySessionStore;
use telemetry::{metrics::AppMetrics, metrics::spawn_metrics_task};
use tower_http::trace::{DefaultOnFailure, DefaultOnRequest, DefaultOnResponse, TraceLayer};
use tracing::Level;

pub fn build_state() -> AppState {
    AppState::new(
        InMemorySessionStore::new(),
        RateLimitService::new(),
        AppMetrics::new(),
    )
}

pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/ready", get(readiness))
        .route("/metrics", get(metrics))
        .route("/api/session/create", post(create_session))
        .route("/ws/upload/{code}", get(upload_ws))
        .route("/ws/download/{code}", get(download_ws))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(telemetry::tracing::make_http_span)
                .on_request(DefaultOnRequest::new().level(Level::DEBUG))
                .on_response(DefaultOnResponse::new().level(Level::INFO))
                .on_failure(DefaultOnFailure::new().level(Level::ERROR)),
        )
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

pub async fn serve_with_shutdown<F>(
    listener: tokio::net::TcpListener,
    app: Router,
    signal: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(signal)
    .await
}

/// What somebody opening the relay's address in a browser sees.
///
/// There is no page here any more (see `docs/decisions.md` entry 17), and the
/// operator is the only person likely to look. A bare 404 at the root of a
/// deployment reads as a broken one, so this says what the host is instead.
/// A 404, not a redirect, so no monitoring check mistakes it for a page.
async fn root() -> (StatusCode, &'static str) {
    (
        StatusCode::NOT_FOUND,
        "This is a Drop relay. There is no web page here; use the drop command-line \
         client: https://github.com/op-q/drop\n",
    )
}

async fn health() -> &'static str {
    "ok"
}

async fn readiness(State(state): State<AppState>) -> StatusCode {
    if state.is_accepting_connections() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
