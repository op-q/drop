mod common;

use std::time::{Duration, Instant};

use api::{build_state, domain::session::Session};
use common::spawn_network_test_server_with_state;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn next_message(
    stream: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Message {
    timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("expected websocket message before timeout")
        .expect("expected websocket stream item")
        .expect("expected successful websocket message")
}

async fn next_json_message_matching<F>(
    stream: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    matcher: F,
) -> Value
where
    F: Fn(&Value) -> bool,
{
    loop {
        let message = next_message(stream).await;

        if !message.is_text() {
            continue;
        }

        let payload: Value =
            serde_json::from_str(&message.into_text().expect("expected websocket text"))
                .expect("expected websocket JSON");

        if matcher(&payload) {
            return payload;
        }
    }
}

async fn next_binary_message(
    stream: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Vec<u8> {
    loop {
        let message = next_message(stream).await;

        if message.is_binary() {
            return message.into_data();
        }
    }
}

#[tokio::test]
async fn upload_socket_relays_file_to_receiver() {
    let code = "ABC123";
    let state = build_state();
    state
        .sessions
        .insert(
            code.to_string(),
            Session {
                filename: "hello.txt".into(),
                file_size: 11,
                created_at: Instant::now(),
                sender_tx: None,
                download_tx: None,
                sender_connected: false,
                receiver_connected: false,
            },
        )
        .await;

    let server = spawn_network_test_server_with_state(state.clone()).await;

    let (mut receiver_ws, _) = connect_async(server.ws_url(&format!("/ws/download/{code}")))
        .await
        .expect("expected receiver websocket connection");

    let receiver_status = next_json_message_matching(&mut receiver_ws, |payload| {
        payload["type"] == "status" && payload["status"] == "waiting_for_sender"
    })
    .await;
    assert_eq!(receiver_status["type"], "status");
    assert_eq!(receiver_status["status"], "waiting_for_sender");

    let (mut sender_ws, _) = connect_async(server.ws_url(&format!("/ws/upload/{code}")))
        .await
        .expect("expected sender websocket connection");

    let sender_status = next_json_message_matching(&mut sender_ws, |payload| {
        payload["type"] == "status" && payload["status"] == "receiver_connected"
    })
    .await;
    assert_eq!(sender_status["type"], "status");
    assert_eq!(sender_status["status"], "receiver_connected");

    sender_ws
        .send(Message::text(
            json!({
                "type": "meta",
                "filename": "hello.txt",
                "file_size": 11,
                "mime_type": "text/plain",
            })
            .to_string(),
        ))
        .await
        .expect("expected sender meta message to be sent");

    let receiver_meta =
        next_json_message_matching(&mut receiver_ws, |payload| payload["type"] == "meta").await;
    assert_eq!(receiver_meta["type"], "meta");
    assert_eq!(receiver_meta["filename"], "hello.txt");
    assert_eq!(receiver_meta["file_size"], 11);

    let sender_sending_status = next_json_message_matching(&mut sender_ws, |payload| {
        payload["type"] == "status" && payload["status"] == "sending"
    })
    .await;
    assert_eq!(sender_sending_status["status"], "sending");

    sender_ws
        .send(Message::binary(b"hello world".to_vec()))
        .await
        .expect("expected binary chunk to be sent");

    let receiver_binary = next_binary_message(&mut receiver_ws).await;
    assert_eq!(receiver_binary, b"hello world");

    sender_ws
        .send(Message::text(json!({ "type": "complete" }).to_string()))
        .await
        .expect("expected complete message to be sent");

    let receiver_complete =
        next_json_message_matching(&mut receiver_ws, |payload| payload["type"] == "complete").await;
    assert_eq!(receiver_complete["type"], "complete");

    let sender_complete = next_json_message_matching(&mut sender_ws, |payload| {
        payload["type"] == "status" && payload["status"] == "transfer_complete"
    })
    .await;
    assert_eq!(sender_complete["status"], "transfer_complete");

    assert!(!state.sessions.contains(code).await);
}
