use api::{
    app_state::AppState,
    build_app, build_state,
    config::{bind_addr_from_env, shutdown_drain_delay_secs, shutdown_max_transfer_wait_secs},
    serve_with_shutdown, start_background_services,
    telemetry::logging,
};
use std::net::SocketAddr;
use tokio::time::{Duration, Instant, sleep};
use tracing::{info, warn};

#[tokio::main]
async fn main() {
    logging::init_logging();

    let state = build_state();
    start_background_services(state.clone());

    let app = build_app(state.clone());

    let bind_addr = bind_addr_from_env();
    let addr: SocketAddr = bind_addr.parse().expect("invalid DROP_BIND_ADDR");

    info!("listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");

    serve_with_shutdown(listener, app, shutdown_signal(state))
        .await
        .expect("server error");
}

async fn shutdown_signal(state: AppState) {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(%error, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => warn!(%error, "failed to install SIGTERM handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    let drain_delay = shutdown_drain_delay_secs();
    let max_transfer_wait = shutdown_max_transfer_wait_secs();

    state.begin_draining();
    info!(
        delay_seconds = drain_delay,
        max_transfer_wait_seconds = max_transfer_wait,
        "shutdown requested; readiness disabled before connection draining"
    );
    sleep(Duration::from_secs(drain_delay)).await;

    let deadline = Instant::now() + Duration::from_secs(max_transfer_wait);

    loop {
        let metrics = state.metrics.snapshot();
        if metrics.active_sessions == 0 && metrics.active_ws_connections == 0 {
            info!("all active transfers drained; stopping server");
            break;
        }

        if Instant::now() >= deadline {
            warn!(
                active_sessions = metrics.active_sessions,
                active_ws_connections = metrics.active_ws_connections,
                "transfer drain deadline reached; stopping server"
            );
            break;
        }

        sleep(Duration::from_secs(1)).await;
    }
}
