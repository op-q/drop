//! The relay's HTTP API: session creation, and the origin it is reached at.
//!
//! Carrying the transfer itself is [`crate::transport`]'s job. The split is
//! not cosmetic: this file is relay-specific by nature — a transfer that needs
//! no server creates no session — while the transport is the part a second
//! carrier has to reimplement.

use std::fmt;

use serde::Deserialize;

/// How to name a relay, for whichever sentence has just explained why one is
/// needed.
///
/// There is no compiled-in relay origin any more. The hosted one this file used
/// to name is gone, and a default naming a dead host is worse than none: it
/// turns "you have not configured a relay" into a connection failure against
/// somebody else's DNS, and — because the value was compiled in — every binary
/// already installed keeps trying it. See `docs/decisions.md` entry 16.
///
/// Shared rather than written out at each site so the two halves cannot drift
/// into telling a person two different things to type.
pub const NAME_A_RELAY: &str = "pass --server or set DROP_SERVER to a relay you run";

#[derive(Debug)]
pub enum ClientError {
    Server(String),
    Transport(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server(message) => write!(formatter, "{message}"),
            Self::Transport(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for ClientError {}

#[derive(Deserialize)]
struct CreateSessionResponse {
    code: String,
}

#[derive(Deserialize)]
struct ErrorResponse {
    message: Option<String>,
}

/// Normalizes a user-supplied server value into an `http(s)://host` origin.
///
/// A bare host is assumed to be HTTPS: defaulting to plaintext would silently
/// downgrade a transfer whose bytes cross the public internet.
pub fn normalize_origin(server: &str) -> String {
    let trimmed = server.trim().trim_end_matches('/');

    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return trimmed.to_string();
    }

    if let Some(rest) = trimmed.strip_prefix("ws://") {
        return format!("http://{rest}");
    }

    if let Some(rest) = trimmed.strip_prefix("wss://") {
        return format!("https://{rest}");
    }

    format!("https://{trimmed}")
}

/// Asks the relay for a one-time session code.
/// Reserves a session and returns its nameplate.
///
/// Only the sealed size crosses here. The filename now travels inside the
/// encrypted metadata blob, so there is nothing to send that the relay would
/// be able to read.
pub fn create_session(origin: &str, ciphertext_size: u64) -> Result<String, ClientError> {
    let url = format!("{origin}/api/session/create");

    let response = ureq::post(&url).send_json(ureq::json!({
        "ciphertext_size": ciphertext_size,
    }));

    match response {
        Ok(response) => response
            .into_json::<CreateSessionResponse>()
            .map(|body| body.code)
            .map_err(|error| {
                ClientError::Transport(format!("could not read the relay's response: {error}"))
            }),
        Err(ureq::Error::Status(status, response)) => {
            let message = response
                .into_json::<ErrorResponse>()
                .ok()
                .and_then(|body| body.message)
                .unwrap_or_else(|| format!("the relay rejected the request with status {status}"));

            Err(ClientError::Server(message))
        }
        Err(error) => Err(ClientError::Transport(format!(
            "could not reach {origin}: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_origin;

    #[test]
    fn assumes_https_for_a_bare_host() {
        assert_eq!(
            normalize_origin("drop.lifbom.com"),
            "https://drop.lifbom.com"
        );
    }

    #[test]
    fn keeps_an_explicit_plaintext_origin_for_local_development() {
        assert_eq!(
            normalize_origin("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080"
        );
    }
}
