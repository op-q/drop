use std::{env, net::IpAddr};

use axum::http::{
    HeaderMap, HeaderValue, Method,
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
pub const SHUTDOWN_DRAIN_DELAY_SECS: u64 = 10;
pub const SHUTDOWN_MAX_TRANSFER_WAIT_SECS: u64 = 3_500;

/// The browser client sends 64 KiB chunks, so this leaves four times the
/// headroom it needs while bounding what a hostile or broken sender can make
/// the relay buffer. Worst case is
/// `MAX_CONCURRENT_SESSIONS * DOWNLOAD_EVENT_CHANNEL_CAPACITY * this`, which
/// must stay under the container memory limit: 100 * 8 * 256 KiB = 200 MiB.
pub const WS_MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// Load balancers and orchestrators cap how long a pod may take to stop, and
/// the honored ceiling differs per platform, so both shutdown phases are
/// overridable without rebuilding the image.
pub fn shutdown_drain_delay_secs() -> u64 {
    env_u64("DROP_SHUTDOWN_DRAIN_DELAY_SECS", SHUTDOWN_DRAIN_DELAY_SECS)
}

pub fn shutdown_max_transfer_wait_secs() -> u64 {
    env_u64(
        "DROP_SHUTDOWN_MAX_TRANSFER_WAIT_SECS",
        SHUTDOWN_MAX_TRANSFER_WAIT_SECS,
    )
}

fn env_u64(name: &str, default: u64) -> u64 {
    match env::var(name) {
        Ok(value) => parse_u64_or(&value, default),
        Err(_) => default,
    }
}

fn parse_u64_or(value: &str, default: u64) -> u64 {
    value.trim().parse().unwrap_or(default)
}

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

pub fn client_ip_from_request(peer_ip: IpAddr, headers: &HeaderMap) -> IpAddr {
    if !env_flag("DROP_TRUST_GCP_X_FORWARDED_FOR") {
        return peer_ip;
    }

    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(gcp_forwarded_client_ip)
        .unwrap_or(peer_ip)
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn gcp_forwarded_client_ip(value: &str) -> Option<IpAddr> {
    let mut addresses = value.rsplit(',').map(str::trim);
    addresses.next()?;
    addresses.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::{gcp_forwarded_client_ip, parse_u64_or};

    #[test]
    fn reads_gcp_appended_client_address_from_forwarded_header() {
        assert_eq!(
            gcp_forwarded_client_ip("198.51.100.7, 203.0.113.8"),
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)))
        );
    }

    #[test]
    fn ignores_untrusted_prefixes_in_forwarded_header() {
        assert_eq!(
            gcp_forwarded_client_ip("192.0.2.99, 198.51.100.7, 203.0.113.8"),
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)))
        );
    }

    #[test]
    fn rejects_incomplete_forwarded_header() {
        assert_eq!(gcp_forwarded_client_ip("198.51.100.7"), None);
    }

    #[test]
    fn reads_shutdown_override_from_padded_value() {
        assert_eq!(parse_u64_or(" 540 ", 3_500), 540);
    }

    #[test]
    fn keeps_default_when_shutdown_override_is_not_a_number() {
        assert_eq!(parse_u64_or("ten-minutes", 3_500), 3_500);
    }
}
