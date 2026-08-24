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
            return message.into_data().to_vec();
        }
    }
}

/// Inserts a session the way `POST /api/session/create` would, with neither
/// peer connected yet.
async fn insert_session(state: &api::app_state::AppState, code: &str, ciphertext_size: u64) {
    state
        .sessions
        .insert(
            code.to_string(),
            Session {
                ciphertext_size,
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
}

/// Waits for a failed session to leave the map.
///
/// `fail_session` queues the error frame before it removes the session, so a
/// client can read the error while the removal is still in flight. Polling
/// keeps the assertion about the outcome rather than about the interleaving.
async fn wait_for_session_removal(state: &api::app_state::AppState, code: &str) {
    for _ in 0..200 {
        if state.sessions.get(code).await.is_none() {
            return;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    panic!("expected the failed session to be removed");
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
                ciphertext_size: file_size,
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
                "version": api::config::ENVELOPE_VERSION,
                "ciphertext_size": file_size,
                "metadata": "00",
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
    let full_budget = state.relay_budget.available_bytes();
    state
        .sessions
        .insert(
            code.to_string(),
            Session {
                ciphertext_size: file_size,
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
                "version": api::config::ENVELOPE_VERSION,
                "ciphertext_size": file_size,
                "metadata": "00",
            })
            .to_string(),
        ))
        .await
        .expect("expected sender meta message to be sent");

    let receiver_meta =
        next_json_message_matching(&mut receiver_ws, |payload| payload["type"] == "meta").await;
    assert_eq!(receiver_meta["type"], "meta");
    assert_eq!(receiver_meta["version"], api::config::ENVELOPE_VERSION);
    assert_eq!(receiver_meta["ciphertext_size"], file_size);
    assert_eq!(receiver_meta["metadata"], "00");

    // The relay forwards the blob without being able to read it, and carries
    // no cleartext description of the payload at all. These two fields are the
    // ones encryption removed; their absence is the property, so assert it
    // rather than trusting that nobody adds them back.
    assert!(receiver_meta.get("filename").is_none());
    assert!(receiver_meta.get("mime_type").is_none());

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

    // Every chunk here was written out to the receiver, so this covers the
    // release path a discarded channel never exercises.
    let deadline = Instant::now() + Duration::from_secs(5);
    while state.relay_budget.available_bytes() != full_budget {
        assert!(
            Instant::now() < deadline,
            "relay budget leaked after a completed transfer: {} of {} bytes never came back",
            full_budget - state.relay_budget.available_bytes(),
            full_budget
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The relay budget must come back after a transfer ends, however it ends.
///
/// Every buffered chunk holds a reservation against a server-wide ceiling. A
/// reservation that is not released is invisible in normal operation and only
/// shows up much later, as a relay that has quietly throttled itself to a
/// standstill, so both the clean and the abandoned path are checked here.
#[tokio::test]
async fn returns_the_relay_budget_after_a_receiver_abandons_a_transfer() {
    let code = "BUDGET";
    let chunk = vec![5_u8; 256 * 1024];
    let file_size = (chunk.len() * 4) as u64;
    let state = build_state();
    let full_budget = state.relay_budget.available_bytes();

    state
        .sessions
        .insert(
            code.to_string(),
            Session {
                ciphertext_size: file_size,
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
                "version": api::config::ENVELOPE_VERSION,
                "ciphertext_size": file_size,
                "metadata": "00",
            })
            .to_string(),
        ))
        .await
        .expect("expected sender meta message to be sent");
    next_json_message_matching(&mut receiver_ws, |payload| payload["type"] == "meta").await;

    sender_ws
        .send(Message::binary(chunk.clone()))
        .await
        .expect("expected first chunk to be written");

    // Walk away mid-transfer, leaving chunks buffered inside the relay.
    drop(receiver_ws);
    let _ = sender_ws.send(Message::binary(chunk)).await;
    drop(sender_ws);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if state.relay_budget.available_bytes() == full_budget
            && !state.sessions.contains(code).await
        {
            break;
        }

        assert!(
            Instant::now() < deadline,
            "relay budget leaked: {} of {} bytes never came back",
            full_budget - state.relay_budget.available_bytes(),
            full_budget
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The two peers may connect in either order, so the key exchange has to
/// survive the receiver arriving first. It did not: the receiver sent its half
/// on connect, the relay had no sender to give it to and dropped it, and the
/// sender then waited for a message that no longer existed. Neither side saw an
/// error, because nothing had gone wrong from either one's point of view.
#[tokio::test]
async fn key_exchange_completes_when_the_receiver_connects_first() {
    let code = "RECVFIRST";
    let state = build_state();
    insert_session(&state, code, 64).await;

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

    // The sender goes first, on the signal that the receiver is there.
    sender_ws
        .send(Message::text(
            json!({ "type": "key_exchange", "message": "5e4de5" }).to_string(),
        ))
        .await
        .expect("expected the sender's half to be sent");

    let forwarded_to_receiver = next_json_message_matching(&mut receiver_ws, |payload| {
        payload["type"] == "key_exchange"
    })
    .await;
    assert_eq!(forwarded_to_receiver["message"], "5e4de5");

    // The receiver replies to it, which is what makes the order safe.
    receiver_ws
        .send(Message::text(
            json!({ "type": "key_exchange", "message": "4ecece" }).to_string(),
        ))
        .await
        .expect("expected the receiver's half to be sent");

    let forwarded_to_sender =
        next_json_message_matching(&mut sender_ws, |payload| payload["type"] == "key_exchange")
            .await;
    assert_eq!(forwarded_to_sender["message"], "4ecece");
}

/// A half that arrives before its peer cannot be delivered and is not held, so
/// the relay says so. The alternative is the silence that hid this for a
/// release: a client that sends on connect otherwise looks like it worked.
#[tokio::test]
async fn a_receiver_key_exchange_before_the_sender_is_refused() {
    let code = "EARLYRECV";
    let state = build_state();
    insert_session(&state, code, 64).await;

    let server = spawn_network_test_server_with_state(state.clone()).await;

    let (mut receiver_ws, _) = connect_async(server.ws_url(&format!("/ws/download/{code}")))
        .await
        .expect("expected receiver websocket connection");

    next_json_message_matching(&mut receiver_ws, |payload| {
        payload["type"] == "status" && payload["status"] == "waiting_for_sender"
    })
    .await;

    receiver_ws
        .send(Message::text(
            json!({ "type": "key_exchange", "message": "5e4de5" }).to_string(),
        ))
        .await
        .expect("expected the receiver's half to be sent");

    let error =
        next_json_message_matching(&mut receiver_ws, |payload| payload["type"] == "error").await;
    assert_eq!(
        error["message"],
        "key exchange arrived before the sender connected"
    );

    wait_for_session_removal(&state, code).await;
    assert_eq!(state.metrics.snapshot().total_transfer_failures, 1);
}

/// The same rule from the other side. The sender already waits for
/// `receiver_connected` before sending, so this is what a client that forgets
/// to sees.
#[tokio::test]
async fn a_sender_key_exchange_before_the_receiver_is_refused() {
    let code = "EARLYSEND";
    let state = build_state();
    insert_session(&state, code, 64).await;

    let server = spawn_network_test_server_with_state(state.clone()).await;

    let (mut sender_ws, _) = connect_async(server.ws_url(&format!("/ws/upload/{code}")))
        .await
        .expect("expected sender websocket connection");

    next_json_message_matching(&mut sender_ws, |payload| {
        payload["type"] == "status" && payload["status"] == "waiting_for_receiver"
    })
    .await;

    sender_ws
        .send(Message::text(
            json!({ "type": "key_exchange", "message": "4ecece" }).to_string(),
        ))
        .await
        .expect("expected the sender's half to be sent");

    let error =
        next_json_message_matching(&mut sender_ws, |payload| payload["type"] == "error").await;
    assert_eq!(
        error["message"],
        "key exchange arrived before the receiver connected"
    );

    wait_for_session_removal(&state, code).await;
    assert_eq!(state.metrics.snapshot().total_transfer_failures, 1);
}
