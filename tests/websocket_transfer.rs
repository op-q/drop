mod common;

use std::time::{Duration, Instant};

use api::{build_state, config::WS_MAX_MESSAGE_BYTES, domain::session::Session};
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

/// A single oversized frame must be refused rather than buffered, because the
/// relay runs under a hard container memory limit that
/// `MAX_CONCURRENT_SESSIONS * DOWNLOAD_EVENT_CHANNEL_CAPACITY` oversized chunks
/// would blow past.
#[tokio::test]
async fn upload_socket_rejects_chunks_over_the_message_cap() {
    let code = "TOOBIG";
    let oversized = vec![7_u8; WS_MAX_MESSAGE_BYTES + 1];
    let file_size = oversized.len() as u64;
    let state = build_state();
    state
        .sessions
        .insert(
            code.to_string(),
            Session {
                filename: "oversized.bin".into(),
                file_size,
                created_at: Instant::now(),
                last_activity: Instant::now(),
                sender_tx: None,
                download_tx: None,
                sender_connected: false,
                receiver_connected: false,
                bytes_relayed: 0,
                receiver_acknowledged_bytes: 0,
                sender_finished: false,
            },
        )
        .await;

    let server = spawn_network_test_server_with_state(state.clone()).await;

    let (mut receiver_ws, _) = connect_async(server.ws_url(&format!("/ws/download/{code}")))
        .await
        .expect("expected receiver websocket connection");
    next_json_message_matching(&mut receiver_ws, |payload| {
        payload["type"] == "status" && payload["status"] == "waiting_for_sender"
    })
    .await;

    let (mut sender_ws, _) = connect_async(server.ws_url(&format!("/ws/upload/{code}")))
        .await
        .expect("expected sender websocket connection");
    next_json_message_matching(&mut sender_ws, |payload| {
        payload["type"] == "status" && payload["status"] == "receiver_connected"
    })
    .await;

    sender_ws
        .send(Message::text(
            json!({
                "type": "meta",
                "filename": "oversized.bin",
                "file_size": file_size,
                "mime_type": "application/octet-stream",
            })
            .to_string(),
        ))
        .await
        .expect("expected sender meta message to be sent");
    next_json_message_matching(&mut receiver_ws, |payload| payload["type"] == "meta").await;

    sender_ws
        .send(Message::binary(oversized))
        .await
        .expect("expected oversized chunk to be written");

    // The relay must tear the session down instead of relaying the chunk. The
    // receiver stream therefore ends, with an error frame rather than any part
    // of the oversized payload.
    let mut saw_error_frame = false;
    while let Ok(Some(item)) = timeout(Duration::from_secs(2), receiver_ws.next()).await {
        let Ok(message) = item else {
            break;
        };

        assert!(
            !message.is_binary(),
            "relay must not forward a chunk larger than WS_MAX_MESSAGE_BYTES"
        );

        if message.is_close() {
            break;
        }

        if let Ok(text) = message.into_text()
            && let Ok(payload) = serde_json::from_str::<Value>(&text)
            && payload["type"] == "error"
        {
            saw_error_frame = true;
        }
    }

    assert!(
        saw_error_frame,
        "receiver must be told the transfer failed instead of hanging"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while state.sessions.contains(code).await {
        assert!(
            Instant::now() < deadline,
            "session must be torn down after an oversized chunk"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn upload_socket_relays_acknowledged_chunks_before_reporting_completion() {
    let code = "ABC123";
    let payload = (0..(1024 * 1024 + 17))
        .map(|index| ((index * 31) % 251) as u8)
        .collect::<Vec<_>>();
    let file_size = payload.len() as u64;
    let state = build_state();
    state
        .sessions
        .insert(
            code.to_string(),
            Session {
                filename: "sample.mkv".into(),
                file_size,
                created_at: Instant::now(),
                last_activity: Instant::now(),
                sender_tx: None,
                download_tx: None,
                sender_connected: false,
                receiver_connected: false,
                bytes_relayed: 0,
                receiver_acknowledged_bytes: 0,
                sender_finished: false,
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
                "filename": "sample.mkv",
                "file_size": file_size,
                "mime_type": "video/x-matroska",
            })
            .to_string(),
        ))
        .await
        .expect("expected sender meta message to be sent");

    let receiver_meta =
        next_json_message_matching(&mut receiver_ws, |payload| payload["type"] == "meta").await;
    assert_eq!(receiver_meta["type"], "meta");
    assert_eq!(receiver_meta["filename"], "sample.mkv");
    assert_eq!(receiver_meta["file_size"], file_size);

    let sender_sending_status = next_json_message_matching(&mut sender_ws, |payload| {
        payload["type"] == "status" && payload["status"] == "sending"
    })
    .await;
    assert_eq!(sender_sending_status["status"], "sending");

    let mut acknowledged = 0_u64;
    for chunk in payload.chunks(64 * 1024) {
        sender_ws
            .send(Message::binary(chunk.to_vec()))
            .await
            .expect("expected binary chunk to be sent");

        let receiver_binary = next_binary_message(&mut receiver_ws).await;
        assert_eq!(receiver_binary, chunk);
        acknowledged += receiver_binary.len() as u64;

        receiver_ws
            .send(Message::text(
                json!({
                    "type": "chunk_ack",
                    "bytes_received": acknowledged,
                })
                .to_string(),
            ))
            .await
            .expect("expected receiver acknowledgement to be sent");

        let acknowledgement = next_json_message_matching(&mut sender_ws, |message| {
            message["type"] == "ack" && message["bytes_received"] == acknowledged
        })
        .await;
        assert_eq!(acknowledgement["bytes_received"], acknowledged);
    }

    assert_eq!(acknowledged, file_size);

    sender_ws
        .send(Message::text(json!({ "type": "complete" }).to_string()))
        .await
        .expect("expected complete message to be sent");

    let receiver_complete =
        next_json_message_matching(&mut receiver_ws, |payload| payload["type"] == "complete").await;
    assert_eq!(receiver_complete["type"], "complete");

    assert!(state.sessions.contains(code).await);
    let premature_sender_completion = timeout(
        Duration::from_millis(100),
        next_json_message_matching(&mut sender_ws, |payload| {
            payload["type"] == "status" && payload["status"] == "transfer_complete"
        }),
    )
    .await;
    assert!(
        premature_sender_completion.is_err(),
        "sender must not see completion before the receiver saves the file"
    );

    receiver_ws
        .send(Message::text(
            json!({
                "type": "complete",
                "bytes_received": file_size,
            })
            .to_string(),
        ))
        .await
        .expect("expected receiver completion acknowledgement to be sent");

    let sender_complete = next_json_message_matching(&mut sender_ws, |payload| {
        payload["type"] == "status" && payload["status"] == "transfer_complete"
    })
    .await;
    assert_eq!(sender_complete["status"], "transfer_complete");

    assert!(!state.sessions.contains(code).await);
}
