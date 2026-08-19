use std::net::{IpAddr, SocketAddr};

use axum::{
    extract::{
        ConnectInfo, Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::HeaderMap,
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, interval, timeout};
use tracing::{Instrument, debug, info, warn};

use crate::{
    app_state::AppState,
    config::{
        MAX_UPLOAD_SIZE_BYTES, MAX_UPLOAD_SIZE_LABEL, PROGRESS_INTERVAL_MS,
        SENDER_EVENT_CHANNEL_CAPACITY, WS_CLOSE_DRAIN_TIMEOUT_SECS, WS_HEARTBEAT_INTERVAL_SECS,
        WS_IDLE_TIMEOUT_SECS, WS_MAX_MESSAGE_BYTES, client_ip_from_request,
    },
    domain::{
        messages::SenderMessage,
        session::{DownloadEvent, SenderEvent},
    },
    errors::AppError,
    services::{
        cleanup_service::remove_expired_sessions,
        session_service::{SenderClaimResult, SessionService},
        transfer_service::TransferService,
    },
    telemetry::tracing::transfer_span,
    ws::protocol,
};

pub async fn upload_ws(
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(code): Path<String>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    state.ensure_accepting_connections()?;
    let client_ip = client_ip_from_request(addr.ip(), &headers);
    remove_expired_sessions(&state).await;
    state
        .rate_limiter
        .check_connection_attempt_limit(client_ip)
        .await?;
    state
        .rate_limiter
        .try_acquire_ws_connection(client_ip)
        .await?;
    state.metrics.record_ws_connection_opened();

    Ok(ws
        .max_message_size(WS_MAX_MESSAGE_BYTES)
        .max_frame_size(WS_MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, code, state, client_ip)))
}

/// Throttles advisory progress notifications.
///
/// Progress carries no protocol meaning, so emitting one per chunk spent
/// serialization and socket wakeups on updates no user interface could show.
struct ProgressThrottle {
    last_emitted: Instant,
}

impl ProgressThrottle {
    fn new() -> Self {
        Self {
            last_emitted: Instant::now(),
        }
    }

    fn should_emit(&mut self) -> bool {
        let now = Instant::now();

        if now.duration_since(self.last_emitted) < Duration::from_millis(PROGRESS_INTERVAL_MS) {
            return false;
        }

        self.last_emitted = now;
        true
    }
}

/// Sends a progress update to both peers over already-resolved channels.
///
/// Delivery is best effort: a full channel means that peer is behind on work
/// that matters more than an advisory update, so the update is dropped rather
/// than allowed to stall the relay.
fn send_progress(
    sender_tx: &mpsc::Sender<SenderEvent>,
    download_tx: &mpsc::Sender<DownloadEvent>,
    code: &str,
    bytes_transferred: u64,
    total_bytes: u64,
) {
    let sender_event = SenderEvent::Progress {
        bytes_transferred,
        total_bytes,
    };
    protocol::log_sender_event(code, &sender_event);
    let _ = sender_tx.try_send(sender_event);

    let download_event = DownloadEvent::Progress {
        bytes_transferred,
        total_bytes,
    };
    protocol::log_download_event(code, &download_event);
    let _ = download_tx.try_send(download_event);
}

async fn handle_socket(socket: WebSocket, code: String, state: AppState, client_ip: IpAddr) {
    let span = transfer_span("upload", &code);
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (sender_tx, mut sender_rx) = mpsc::channel::<SenderEvent>(SENDER_EVENT_CHANNEL_CAPACITY);

    let receiver_already_connected =
        match SessionService::claim_sender(&state, &code, sender_tx.clone()).await {
            SenderClaimResult::InvalidCode => {
                let _ = ws_sender
                    .send(Message::Text(
                        r#"{"type":"error","message":"invalid session code"}"#.into(),
                    ))
                    .await;
                let _ = ws_sender.close().await;
                state.rate_limiter.release_ws_connection(client_ip).await;
                state.metrics.record_ws_connection_closed();
                return;
            }
            SenderClaimResult::AlreadyConnected => {
                let _ = ws_sender
                    .send(Message::Text(
                        r#"{"type":"error","message":"sender already connected"}"#.into(),
                    ))
                    .await;
                let _ = ws_sender.close().await;
                state.rate_limiter.release_ws_connection(client_ip).await;
                state.metrics.record_ws_connection_closed();
                return;
            }
            SenderClaimResult::Accepted { receiver_connected } => receiver_connected,
        };

    info!(session_code = %code, client_ip = %client_ip, "sender connected");

    if receiver_already_connected {
        TransferService::send_sender(&state, &code, SenderEvent::Status("receiver_connected"))
            .await;
    } else {
        TransferService::send_sender(&state, &code, SenderEvent::Status("waiting_for_receiver"))
            .await;
    }

    let send_task = tokio::spawn(
        async move {
            let mut heartbeat = interval(Duration::from_secs(WS_HEARTBEAT_INTERVAL_SECS));

            loop {
                tokio::select! {
                    _ = heartbeat.tick() => {
                        if ws_sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                            break;
                        }
                    }
                    maybe_event = sender_rx.recv() => {
                        let Some(event) = maybe_event else {
                            break;
                        };

                        match event {
                            SenderEvent::Status(status) => {
                                let msg = serde_json::json!({
                                    "type": "status",
                                    "status": status
                                });

                                let terminal = matches!(status, "transfer_complete" | "cancelled");
                                if ws_sender
                                    .send(Message::Text(msg.to_string().into()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }

                                if terminal {
                                    let _ = ws_sender.send(Message::Close(None)).await;
                                    break;
                                }
                            }
                            SenderEvent::Progress {
                                bytes_transferred,
                                total_bytes,
                            } => {
                                let msg = serde_json::json!({
                                    "type": "progress",
                                    "bytes_transferred": bytes_transferred,
                                    "total_bytes": total_bytes
                                });

                                if ws_sender
                                    .send(Message::Text(msg.to_string().into()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            SenderEvent::Acknowledgement { bytes_received } => {
                                let msg = serde_json::json!({
                                    "type": "ack",
                                    "bytes_received": bytes_received
                                });

                                if ws_sender
                                    .send(Message::Text(msg.to_string().into()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            SenderEvent::Error(message) => {
                                let msg = serde_json::json!({
                                    "type": "error",
                                    "message": message
                                });

                                let _ = ws_sender
                                    .send(Message::Text(msg.to_string().into()))
                                    .await;
                                break;
                            }
                        }
                    }
                }
            }
        }
        .instrument(span.clone()),
    );

    let state_for_recv = state.clone();
    let code_for_recv = code.clone();
    let sender_tx_for_recv = sender_tx;

    let recv_task = tokio::spawn(
        async move {
            let idle_timeout = Duration::from_secs(WS_IDLE_TIMEOUT_SECS);
            // Both peer channels are resolved once, when metadata arrives, and
            // then held for the rest of the transfer. Looking them up per chunk
            // meant taking the global session lock and cloning the whole
            // `Session` several times for every chunk relayed.
            let mut relay_target: Option<mpsc::Sender<DownloadEvent>> = None;
            let mut expected_file_size = None;
            let mut bytes_received = 0_u64;
            let mut progress = ProgressThrottle::new();
            // Set when the sender completes normally, so teardown knows this
            // socket is owed a closing handshake rather than being abandoned.
            let mut sender_completed = false;

            loop {
                let result = match timeout(idle_timeout, ws_receiver.next()).await {
                    Ok(Some(result)) => result,
                    Ok(None) => break,
                    Err(_) => {
                        TransferService::fail_session(
                            &state_for_recv,
                            &code_for_recv,
                            Some("transfer timed out due to inactivity"),
                            Some("transfer timed out due to inactivity"),
                            "upload socket timed out",
                        )
                        .await;
                        break;
                    }
                };

                match result {
                    Ok(Message::Text(text)) => match serde_json::from_str::<SenderMessage>(&text) {
                        Ok(message) => {
                            protocol::log_incoming_sender_message(&code_for_recv, &message);

                            match message {
                                SenderMessage::Meta {
                                    filename,
                                    file_size,
                                    mime_type,
                                } => {
                                    let Some(session_file_size) =
                                        SessionService::session_file_size(
                                            &state_for_recv,
                                            &code_for_recv,
                                        )
                                        .await
                                    else {
                                        break;
                                    };

                                    if file_size != session_file_size {
                                        TransferService::fail_session(
                                            &state_for_recv,
                                            &code_for_recv,
                                            Some(
                                                "meta file size does not match the created session",
                                            ),
                                            None,
                                            "sender meta size mismatch",
                                        )
                                        .await;
                                        break;
                                    }

                                    if file_size > MAX_UPLOAD_SIZE_BYTES {
                                        let message = format!(
                                            "file size exceeds the {} upload limit",
                                            MAX_UPLOAD_SIZE_LABEL
                                        );
                                        TransferService::fail_session(
                                            &state_for_recv,
                                            &code_for_recv,
                                            Some(&message),
                                            None,
                                            "sender meta exceeded upload limit",
                                        )
                                        .await;
                                        break;
                                    }

                                    if !SessionService::receiver_connected(
                                        &state_for_recv,
                                        &code_for_recv,
                                    )
                                    .await
                                    {
                                        TransferService::fail_session(
                                            &state_for_recv,
                                            &code_for_recv,
                                            Some("receiver is not connected"),
                                            None,
                                            "sender started before receiver connected",
                                        )
                                        .await;
                                        break;
                                    }

                                    let Some(download_tx) = SessionService::download_tx(
                                        &state_for_recv,
                                        &code_for_recv,
                                    )
                                    .await
                                    else {
                                        TransferService::fail_session(
                                            &state_for_recv,
                                            &code_for_recv,
                                            Some("receiver is not connected"),
                                            None,
                                            "receiver channel missing at sender meta",
                                        )
                                        .await;
                                        break;
                                    };

                                    expected_file_size = Some(file_size);
                                    let _ = SessionService::touch_session(
                                        &state_for_recv,
                                        &code_for_recv,
                                    )
                                    .await;

                                    TransferService::send_receiver(
                                        &state_for_recv,
                                        &code_for_recv,
                                        DownloadEvent::Meta {
                                            filename,
                                            file_size,
                                            mime_type,
                                        },
                                    )
                                    .await;

                                    TransferService::send_sender(
                                        &state_for_recv,
                                        &code_for_recv,
                                        SenderEvent::Status("sending"),
                                    )
                                    .await;
                                    send_progress(
                                        &sender_tx_for_recv,
                                        &download_tx,
                                        &code_for_recv,
                                        0,
                                        file_size,
                                    );

                                    relay_target = Some(download_tx);
                                }

                                SenderMessage::Complete => {
                                    if Some(bytes_received) != expected_file_size {
                                        TransferService::fail_session(
                                            &state_for_recv,
                                            &code_for_recv,
                                            Some("sent bytes did not match declared file size"),
                                            Some(
                                                "transfer ended before declared file size was sent",
                                            ),
                                            "sender completed with mismatched byte count",
                                        )
                                        .await;
                                        break;
                                    }

                                    if !SessionService::mark_sender_finished(
                                        &state_for_recv,
                                        &code_for_recv,
                                    )
                                    .await
                                    {
                                        TransferService::fail_session(
                                            &state_for_recv,
                                            &code_for_recv,
                                            Some("transfer could not be finalized"),
                                            Some("transfer could not be finalized"),
                                            "sender completion state was invalid",
                                        )
                                        .await;
                                        break;
                                    }

                                    if let Some(download_tx) = relay_target.as_ref() {
                                        send_progress(
                                            &sender_tx_for_recv,
                                            download_tx,
                                            &code_for_recv,
                                            bytes_received,
                                            bytes_received,
                                        );
                                    }

                                    TransferService::send_receiver(
                                        &state_for_recv,
                                        &code_for_recv,
                                        DownloadEvent::Complete,
                                    )
                                    .await;
                                    TransferService::send_sender(
                                        &state_for_recv,
                                        &code_for_recv,
                                        SenderEvent::Status("awaiting_receiver"),
                                    )
                                    .await;
                                    sender_completed = true;
                                    break;
                                }

                                SenderMessage::Cancel => {
                                    TransferService::send_sender(
                                        &state_for_recv,
                                        &code_for_recv,
                                        SenderEvent::Status("cancelled"),
                                    )
                                    .await;
                                    TransferService::fail_session(
                                        &state_for_recv,
                                        &code_for_recv,
                                        None,
                                        Some("sender cancelled"),
                                        "sender cancelled transfer",
                                    )
                                    .await;
                                    break;
                                }
                            }
                        }

                        Err(err) => {
                            warn!(
                                "invalid sender control message for {}: {}",
                                code_for_recv, err
                            );
                            TransferService::send_sender(
                                &state_for_recv,
                                &code_for_recv,
                                SenderEvent::Error("invalid control message".into()),
                            )
                            .await;
                        }
                    },

                    Ok(Message::Binary(bytes)) => {
                        let Some(download_tx) = relay_target.as_ref() else {
                            TransferService::send_sender(
                                &state_for_recv,
                                &code_for_recv,
                                SenderEvent::Error("send meta before binary chunks".into()),
                            )
                            .await;
                            continue;
                        };

                        let chunk_len = bytes.len() as u64;
                        bytes_received = bytes_received.saturating_add(chunk_len);
                        protocol::log_incoming_upload_chunk(
                            &code_for_recv,
                            bytes.len(),
                            bytes_received,
                            expected_file_size,
                        );

                        let exceeds_expected = expected_file_size
                            .map(|expected| bytes_received > expected)
                            .unwrap_or(true);

                        if bytes_received > MAX_UPLOAD_SIZE_BYTES || exceeds_expected {
                            TransferService::fail_session(
                                &state_for_recv,
                                &code_for_recv,
                                Some("upload exceeded the allowed file size"),
                                Some("upload exceeded allowed size"),
                                "sender exceeded allowed size",
                            )
                            .await;
                            break;
                        }

                        if !SessionService::record_relayed_bytes(
                            &state_for_recv,
                            &code_for_recv,
                            bytes_received,
                        )
                        .await
                        {
                            TransferService::fail_session(
                                &state_for_recv,
                                &code_for_recv,
                                Some("transfer byte accounting failed"),
                                Some("transfer byte accounting failed"),
                                "could not record relayed bytes",
                            )
                            .await;
                            break;
                        }

                        // Waiting here is the backpressure that keeps total
                        // buffered file data inside the server-wide ceiling.
                        // The reservation travels with the chunk and is
                        // released once the receiver socket has written it.
                        let Some(reservation) =
                            state_for_recv.relay_budget.reserve(bytes.len()).await
                        else {
                            TransferService::fail_session(
                                &state_for_recv,
                                &code_for_recv,
                                Some("chunk is larger than the relay can buffer"),
                                Some("sender chunk exceeded the relay buffer"),
                                "chunk exceeded the relay budget",
                            )
                            .await;
                            break;
                        };

                        if download_tx
                            .send(DownloadEvent::Chunk {
                                data: bytes.to_vec(),
                                reservation,
                            })
                            .await
                            .is_err()
                        {
                            TransferService::fail_session(
                                &state_for_recv,
                                &code_for_recv,
                                Some("receiver disconnected"),
                                None,
                                "receiver disconnected while receiving chunk",
                            )
                            .await;
                            break;
                        }

                        TransferService::record_bytes_relayed(&state_for_recv, chunk_len);

                        if progress.should_emit() {
                            send_progress(
                                &sender_tx_for_recv,
                                download_tx,
                                &code_for_recv,
                                bytes_received,
                                expected_file_size.unwrap_or(bytes_received),
                            );
                        }
                    }

                    Ok(Message::Close(_)) => {
                        TransferService::fail_session(
                            &state_for_recv,
                            &code_for_recv,
                            None,
                            Some("sender disconnected"),
                            "sender closed websocket",
                        )
                        .await;
                        break;
                    }

                    Ok(_) => {}

                    Err(err) => {
                        warn!("sender socket error for {}: {}", code_for_recv, err);
                        TransferService::fail_session(
                            &state_for_recv,
                            &code_for_recv,
                            None,
                            Some("sender socket error"),
                            "sender websocket error",
                        )
                        .await;
                        break;
                    }
                }
            }

            // The relay still owes this sender a terminal status and a `Close`
            // frame, both written by the send task. Dropping `ws_receiver` here
            // would stop reading a socket that is still open, so the peer's
            // reply to that `Close` would sit unread, and closing a socket with
            // data queued sends RST instead of FIN. The sender reports that as
            // `Connection reset by peer` after a transfer that actually
            // succeeded.
            //
            // The wait cannot be a single short deadline: the send task cannot
            // write `transfer_complete` until the receiver has finished writing
            // the file, which is unbounded work. So wait for the send task to
            // finish first — it drops the event receiver as it exits — and only
            // then give the peer a brief window to answer. Both stages are
            // bounded, so a peer that goes quiet cannot hold this task and its
            // per-IP connection slot open.
            if sender_completed {
                if timeout(
                    Duration::from_secs(WS_IDLE_TIMEOUT_SECS),
                    sender_tx_for_recv.closed(),
                )
                .await
                .is_err()
                {
                    debug!(
                        session_code = %code_for_recv,
                        "send task did not finish before the closing handshake wait expired"
                    );
                }

                let answered = timeout(Duration::from_secs(WS_CLOSE_DRAIN_TIMEOUT_SECS), async {
                    while let Some(Ok(message)) = ws_receiver.next().await {
                        if matches!(message, Message::Close(_)) {
                            return;
                        }
                    }
                })
                .await;

                if answered.is_err() {
                    debug!(
                        session_code = %code_for_recv,
                        "sender did not answer the closing handshake before the drain deadline"
                    );
                }
            }
        }
        .instrument(span),
    );

    let _ = tokio::join!(send_task, recv_task);

    if SessionService::remove_session(&state, &code)
        .await
        .is_some()
    {
        state.metrics.record_transfer_failed();
        warn!(
            session_code = %code,
            "upload session ended without an explicit terminal event"
        );
    }

    state.rate_limiter.release_ws_connection(client_ip).await;
    state.metrics.record_ws_connection_closed();
    info!(session_code = %code, client_ip = %client_ip, "upload socket closed");
}
