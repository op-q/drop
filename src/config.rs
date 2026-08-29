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
pub const DOWNLOAD_EVENT_CHANNEL_CAPACITY: usize = 32;
pub const RECEIVER_SEND_TIMEOUT_SECS: u64 = 10;
pub const WS_HEARTBEAT_INTERVAL_SECS: u64 = 15;
pub const WS_IDLE_TIMEOUT_SECS: u64 = 45;
pub const SHUTDOWN_DRAIN_DELAY_SECS: u64 = 10;
pub const SHUTDOWN_MAX_TRANSFER_WAIT_SECS: u64 = 3_500;

/// How long the upload socket keeps reading after the relay has written its
/// closing frame, waiting for the peer to answer it.
///
/// A WebSocket close is a two-way handshake. If the relay stops reading as soon
/// as it has written its own `Close`, the peer's reply lands in a socket nobody
/// is draining, and closing a socket with unread data queued sends RST instead
/// of FIN — which the sender reports as `Connection reset by peer` after a
/// transfer that actually succeeded. This bounds how long that courtesy lasts,
/// so a peer that never answers cannot hold the task and its per-IP connection
/// slot open.
pub const WS_CLOSE_DRAIN_TIMEOUT_SECS: u64 = 2;

/// The chunk size clients should send. Larger chunks mean fewer WebSocket
/// frames, fewer wakeups, and fewer control messages per transferred byte, so
/// a transfer is no longer bottlenecked on per-chunk overhead.
pub const RECOMMENDED_CHUNK_BYTES: usize = 1024 * 1024;

/// Accepted frame ceiling. This is deliberately larger than
/// `RECOMMENDED_CHUNK_BYTES` so a client that pads or slightly overshoots a
/// 1 MiB chunk is not disconnected, while a hostile sender still cannot make
/// the relay buffer an unbounded frame.
pub const WS_MAX_MESSAGE_BYTES: usize = RECOMMENDED_CHUNK_BYTES + 64 * 1024;

/// Total bytes of relayed file data that may sit in memory across *all*
/// sessions at once, enforced by [`crate::services::relay_budget::RelayBudget`].
///
/// Per-session buffering is bounded by `DOWNLOAD_EVENT_CHANNEL_CAPACITY`
/// chunks, but the old per-session-only bound multiplied out to
/// `MAX_CONCURRENT_SESSIONS * capacity * chunk`, which cannot grow with the
/// chunk size and stay inside the container memory limit. A shared budget
/// decouples the two: one transfer may use a 32 MiB window, while a hundred
/// concurrent transfers share this ceiling instead of multiplying it. The
/// value keeps the same worst case the 256 KiB-frame relay had.
pub const RELAY_BUDGET_BYTES: usize = 200 * 1024 * 1024;

/// Progress notifications are throttled to this interval. They are advisory UI
/// updates, so emitting one per chunk only burned serialization and socket
/// wakeups that the actual file bytes needed.
pub const PROGRESS_INTERVAL_MS: u64 = 200;

/// The envelope version the relay will carry.
///
/// The relay cannot open the envelope, but it does refuse a version it does
/// not recognise, so an incompatible pair fails at the relay rather than
/// halfway through a transfer. Duplicated from `ENVELOPE_VERSION` in the CLI's
/// `crypto::envelope`, because the relay cannot depend on the client crate;
/// `envelope_version_matches_the_client` in `cli/tests/protocol.rs` fails if
/// the two ever drift apart.
pub const ENVELOPE_VERSION: u8 = 1;

/// Ceiling on an opaque client-supplied field the relay forwards without
/// understanding — the sealed metadata blob and the key-exchange messages.
///
/// The relay cannot inspect these, which is the point, so it bounds them
/// instead. Without a limit they are an unmetered side channel: two clients
/// could pass arbitrary data through a session while relaying almost no
/// chunks, outside the transfer accounting entirely. Real values are well
/// under a kilobyte.
pub const MAX_OPAQUE_FIELD_BYTES: usize = 8 * 1024;

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
