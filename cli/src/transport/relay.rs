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

/// What a frame means to a sender that is still waiting for its peer.
enum WhileWaiting {
    /// The receiver has joined.
    PeerArrived,
    /// The relay refused the transfer, and this is what it said.
    Refused(String),
    /// Progress, heartbeats, the relay narrating itself. Keep waiting.
    KeepWaiting,
}

/// Split out from the wait so it can be tested without a relay to talk to.
fn while_waiting(payload: &Value) -> WhileWaiting {
    match payload["type"].as_str() {
        Some("status") if payload["status"].as_str() == Some("receiver_connected") => {
            WhileWaiting::PeerArrived
        }
        Some("error") => WhileWaiting::Refused(
            payload["message"]
                .as_str()
                .unwrap_or("the relay reported an error")
                .to_string(),
        ),
        _ => WhileWaiting::KeepWaiting,
    }
}

impl Transport for RelayTransport {
    /// A relay session outlives neither peer but precedes both, so the sender
    /// has to be told when the other side turns up.
    async fn await_peer(&mut self) -> Result<(), TransportError> {
        while let Some(frame) = self.receive().await? {
            let Frame::Control(payload) = frame else {
                continue;
            };

            match while_waiting(&payload) {
                WhileWaiting::PeerArrived => return Ok(()),
                WhileWaiting::Refused(message) => return Err(TransportError::Refused(message)),
                WhileWaiting::KeepWaiting => {}
            }
        }

        Err(TransportError::Io(
            "the relay closed the connection before a receiver joined".into(),
        ))
    }

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
    use super::{WhileWaiting, encode_nameplate, websocket_origin, while_waiting};
    use serde_json::json;

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
    fn only_the_receivers_arrival_ends_the_wait() {
        assert!(matches!(
            while_waiting(&json!({ "type": "status", "status": "receiver_connected" })),
            WhileWaiting::PeerArrived
        ));

        for narration in [
            json!({ "type": "status", "status": "waiting_for_receiver" }),
            json!({ "type": "progress", "bytes_transferred": 0 }),
            json!({ "type": "status", "status": "sending" }),
        ] {
            assert!(
                matches!(while_waiting(&narration), WhileWaiting::KeepWaiting),
                "the relay narrating itself is not a receiver: {narration}"
            );
        }
    }

    #[test]
    fn a_refusal_is_passed_through_in_the_relays_own_words() {
        let WhileWaiting::Refused(message) =
            while_waiting(&json!({ "type": "error", "message": "invalid session code" }))
        else {
            panic!("an error frame is a refusal");
        };

        assert_eq!(message, "invalid session code");
    }

    #[test]
    fn strips_anything_a_nameplate_could_not_contain() {
        assert_eq!(encode_nameplate("7F2A91"), "7F2A91");
        assert_eq!(encode_nameplate("7F2A91/../admin"), "7F2A91admin");
    }
}
