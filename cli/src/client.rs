//! Transport to a Drop relay: session creation over HTTP, transfer over
//! WebSocket.

use std::fmt;

use serde::Deserialize;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async, tungstenite::client::IntoClientRequest,
};

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The hosted relay's API origin.
///
/// This is deliberately not `drop.lifbom.com`. The hosted instance is a split
/// deployment: that host serves the browser client and `install.sh` as static
/// files, while the relay itself answers on the API origin. Pointing the CLI at
/// the site host makes `POST /api/session/create` return the static host's 404.
pub const DEFAULT_SERVER: &str = "https://api.drop.lifbom.com";

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

fn websocket_origin(origin: &str) -> String {
    if let Some(rest) = origin.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = origin.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        origin.to_string()
    }
}

/// Asks the relay for a one-time session code.
pub fn create_session(origin: &str, filename: &str, file_size: u64) -> Result<String, ClientError> {
    let url = format!("{origin}/api/session/create");

    let response = ureq::post(&url).send_json(ureq::json!({
        "filename": filename,
        "file_size": file_size,
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

async fn open(origin: &str, path: &str) -> Result<Socket, ClientError> {
    let url = format!("{}{}", websocket_origin(origin), path);

    let request = url
        .as_str()
        .into_client_request()
        .map_err(|error| ClientError::Transport(format!("invalid relay URL {url}: {error}")))?;

    let (socket, _) = connect_async(request).await.map_err(|error| {
        ClientError::Transport(format!("could not open a transfer connection: {error}"))
    })?;

    Ok(socket)
}

pub async fn open_upload(origin: &str, code: &str) -> Result<Socket, ClientError> {
    open(origin, &format!("/ws/upload/{}", encode_code(code))).await
}

pub async fn open_download(origin: &str, code: &str) -> Result<Socket, ClientError> {
    open(origin, &format!("/ws/download/{}", encode_code(code))).await
}

/// Session codes are six uppercase hex characters, so anything outside that set
/// is a user typo rather than something to percent-encode and send onward.
fn encode_code(code: &str) -> String {
    code.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{normalize_origin, websocket_origin};

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

    #[test]
    fn maps_origins_onto_matching_websocket_schemes() {
        assert_eq!(
            websocket_origin("https://drop.lifbom.com"),
            "wss://drop.lifbom.com"
        );
        assert_eq!(
            websocket_origin("http://localhost:8080"),
            "ws://localhost:8080"
        );
    }
}
