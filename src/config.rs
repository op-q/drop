use std::env;

use axum::http::{
    HeaderValue, Method,
    header::{ACCEPT, CONTENT_TYPE},
};
use tower_http::cors::{AllowOrigin, CorsLayer};

pub const GIBIBYTE: u64 = 1024 * 1024 * 1024;
pub const MAX_UPLOAD_SIZE_BYTES: u64 = 4 * GIBIBYTE;
pub const MAX_UPLOAD_SIZE_LABEL: &str = "4 GB";
pub const MAX_CONCURRENT_SESSIONS: usize = 100;
pub const MAX_WS_CONNECTIONS_PER_IP: usize = 4;
pub const SESSION_CREATION_LIMIT_PER_MINUTE: usize = 10;
pub const CONNECTION_ATTEMPT_LIMIT_PER_MINUTE: usize = 30;
pub const SESSION_TTL_SECS: u64 = 5 * 60;
pub const CLEANUP_INTERVAL_SECS: u64 = 30;
pub const SENDER_EVENT_CHANNEL_CAPACITY: usize = 16;
pub const DOWNLOAD_EVENT_CHANNEL_CAPACITY: usize = 8;
pub const RECEIVER_SEND_TIMEOUT_SECS: u64 = 10;
pub const WS_HEARTBEAT_INTERVAL_SECS: u64 = 15;
pub const WS_IDLE_TIMEOUT_SECS: u64 = 45;

pub fn bind_addr_from_env() -> String {
    if let Ok(bind_addr) = env::var("DROP_BIND_ADDR") {
        return bind_addr;
    }

    if let Ok(port) = env::var("PORT") {
        return format!("0.0.0.0:{port}");
    }

    "0.0.0.0:8080".into()
}

pub fn cors_layer_from_env() -> CorsLayer {
    let layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([CONTENT_TYPE, ACCEPT]);

    let origins = env::var("DROP_ALLOWED_ORIGINS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .map(|origin| {
                    HeaderValue::from_str(origin)
                        .expect("DROP_ALLOWED_ORIGINS contains an invalid origin value")
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if origins.is_empty() {
        layer
    } else {
        layer.allow_origin(AllowOrigin::list(origins))
    }
}
