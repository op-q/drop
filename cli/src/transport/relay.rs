//! The relay transport: a WebSocket to a Drop relay.
//!
//! This is the carrier every transfer used before there was a choice, and it
//! stays the one that works everywhere — browsers can reach it, and so can a
//! network that blocks everything but outbound TLS.
//!
//! Session creation lives in [`crate::client`] rather than here: it is the
//! relay's HTTP API, it happens before a code exists, and the sender needs its
//! answer to build the code in the first place.

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

use super::{Frame, Transport, TransportError};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct RelayTransport {
    socket: Socket,
}

/// Connects the sending half of a transfer to a session the relay has already
/// allocated.
pub async fn connect_sender(
    origin: &str,
    nameplate: &str,
) -> Result<RelayTransport, TransportError> {
    open(
        origin,
        &format!("/ws/upload/{}", encode_nameplate(nameplate)),
    )
    .await
}

/// Connects the receiving half.
pub async fn connect_receiver(
    origin: &str,
    nameplate: &str,
) -> Result<RelayTransport, TransportError> {
    open(
        origin,
        &format!("/ws/download/{}", encode_nameplate(nameplate)),
    )
    .await
}

async fn open(origin: &str, path: &str) -> Result<RelayTransport, TransportError> {
    let url = format!("{}{}", websocket_origin(origin), path);

    let request = url
        .as_str()
        .into_client_request()
        .map_err(|error| TransportError::Connect(format!("invalid relay URL {url}: {error}")))?;

    let (socket, _) = connect_async(request).await.map_err(|error| {
        TransportError::Connect(format!("could not open a transfer connection: {error}"))
    })?;

    Ok(RelayTransport { socket })
}

impl Transport for RelayTransport {
    async fn send_control(&mut self, frame: Value) -> Result<(), TransportError> {
        self.socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .map_err(|error| TransportError::Io(error.to_string()))
    }

    async fn send_chunk(&mut self, chunk: Vec<u8>) -> Result<(), TransportError> {
        self.socket
            .send(Message::Binary(chunk.into()))
            .await
            .map_err(|error| TransportError::Io(error.to_string()))
    }

    async fn receive(&mut self) -> Result<Option<Frame>, TransportError> {
        // Ping, pong, and the frame fragments tungstenite reassembles are the
        // WebSocket's own business. Looping here rather than returning them is
        // what keeps a heartbeat from looking like a message to the transfer.
        while let Some(message) = self.socket.next().await {
            let message = message.map_err(|error| TransportError::Io(error.to_string()))?;

            match message {
                Message::Text(text) => {
                    let value = serde_json::from_str(&text).map_err(|error| {
                        TransportError::Malformed(format!(
                            "the peer sent a control frame that is not JSON: {error}"
                        ))
                    })?;

                    return Ok(Some(Frame::Control(value)));
                }
                Message::Binary(data) => return Ok(Some(Frame::Chunk(data.into()))),
                Message::Close(_) => return Ok(None),
                _ => {}
            }
        }

        Ok(None)
    }

    async fn close(&mut self) {
        let _ = self.socket.close(None).await;
    }
}

/// Nameplates are six uppercase hex characters, so anything outside that set
/// is a user typo rather than something to percent-encode and send onward.
fn encode_nameplate(nameplate: &str) -> String {
    nameplate
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect()
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

#[cfg(test)]
mod tests {
    use super::{encode_nameplate, websocket_origin};

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

    #[test]
    fn strips_anything_a_nameplate_could_not_contain() {
        assert_eq!(encode_nameplate("7F2A91"), "7F2A91");
        assert_eq!(encode_nameplate("7F2A91/../admin"), "7F2A91admin");
    }
}
