mod app_state;
mod domain;
mod routes;

use axum::{
    response::Html,
    routing::{get, post},
    Router,
};
use app_state::AppState;
use routes::{
    download_ws::download_ws,
    sessions::create_session,
    upload_ws::upload_ws,
};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::sync::Mutex;
use tower_http::services::ServeDir;
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let state = AppState {
        sessions: Arc::new(Mutex::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/api/session/create", post(create_session))
        .route("/ws/upload/:code", get(upload_ws))
        .route("/ws/download/:code", get(download_ws))
        .nest_service("/web", ServeDir::new("web"))
        .with_state(state);

    let addr: SocketAddr = "0.0.0.0:8080".parse().expect("invalid bind address");

    info!("listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");

    axum::serve(listener, app)
        .await
        .expect("server error");
}

async fn index() -> Html<&'static str> {
    Html(r#"<html><body><a href="/web/index.html">open drop</a></body></html>"#)
}

async fn health() -> &'static str {
    "ok"
}