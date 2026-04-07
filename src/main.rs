use api::{
    build_app, build_state, config::bind_addr_from_env, serve, start_background_services,
    telemetry::logging,
};
use std::net::SocketAddr;
use tracing::info;

#[tokio::main]
async fn main() {
    logging::init_logging();

    let state = build_state();
    start_background_services(state.clone());

    let app = build_app(state);

    let bind_addr = bind_addr_from_env();
    let addr: SocketAddr = bind_addr.parse().expect("invalid DROP_BIND_ADDR");

    info!("listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");

    serve(listener, app).await.expect("server error");
}
