use std::net::{IpAddr, SocketAddr};

use axum::{
    extract::{
        ConnectInfo, Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{Duration, interval, timeout};
use tracing::{Instrument, info, warn};

use crate::{
    app_state::AppState,
    config::{
        MAX_UPLOAD_SIZE_BYTES, MAX_UPLOAD_SIZE_LABEL, SENDER_EVENT_CHANNEL_CAPACITY,
        WS_HEARTBEAT_INTERVAL_SECS, WS_IDLE_TIMEOUT_SECS,
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
    Path(code): Path<String>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    remove_expired_sessions(&state).await;
    state
        .rate_limiter
        .check_connection_attempt_limit(addr.ip())
        .await?;
    state
        .rate_limiter
        .try_acquire_ws_connection(addr.ip())
        .await?;
    state.metrics.record_ws_connection_opened();

    Ok(ws.on_upgrade(move |socket| handle_socket(socket, code, state, addr.ip())))
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
                        if ws_sender.send(Message::Ping(Vec::new())).await.is_err() {
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

                                if ws_sender
                                    .send(Message::Text(msg.to_string()))
                                    .await
                                    .is_err()
                                {
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
                                    .send(Message::Text(msg.to_string()))
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

                                let _ = ws_sender.send(Message::Text(msg.to_string())).await;
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

    let recv_task = tokio::spawn(
        async move {
            let idle_timeout = Duration::from_secs(WS_IDLE_TIMEOUT_SECS);
            let mut received_meta = false;
            let mut expected_file_size = None;
            let mut bytes_received = 0_u64;

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
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<SenderMessage>(&text) {
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
                                    Some("meta file size does not match the created session"),
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

                                        received_meta = true;
                                        expected_file_size = Some(file_size);

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
                                        TransferService::try_send_sender(
                                            &state_for_recv,
                                            &code_for_recv,
                                            SenderEvent::Progress {
                                                bytes_transferred: 0,
                                                total_bytes: file_size,
                                            },
                                        )
                                        .await;
                                        TransferService::try_send_receiver(
                                            &state_for_recv,
                                            &code_for_recv,
                                            DownloadEvent::Progress {
                                                bytes_transferred: 0,
                                                total_bytes: file_size,
                                            },
                                        )
                                        .await;
                                    }

                                    SenderMessage::Complete => {
                                        if Some(bytes_received) != expected_file_size {
                                            TransferService::fail_session(
                                    &state_for_recv,
                                    &code_for_recv,
                                    Some("sent bytes did not match declared file size"),
                                    Some("transfer ended before declared file size was sent"),
                                    "sender completed with mismatched byte count",
                                )
                                .await;
                                            break;
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
                                            SenderEvent::Status("transfer_complete"),
                                        )
                                        .await;
                                        TransferService::complete_session(
                                            &state_for_recv,
                                            &code_for_recv,
                                            bytes_received,
                                        )
                                        .await;
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
                        }
                    }

                    Ok(Message::Binary(bytes)) => {
                        if !received_meta {
                            TransferService::send_sender(
                                &state_for_recv,
                                &code_for_recv,
                                SenderEvent::Error("send meta before binary chunks".into()),
                            )
                            .await;
                            continue;
                        }

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

                        if let Some(download_tx) =
                            SessionService::download_tx(&state_for_recv, &code_for_recv).await
                        {
                            if download_tx
                                .send(DownloadEvent::Chunk(bytes.to_vec()))
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
                        } else {
                            TransferService::fail_session(
                                &state_for_recv,
                                &code_for_recv,
                                Some("receiver disconnected"),
                                None,
                                "receiver missing during chunk relay",
                            )
                            .await;
                            break;
                        }

                        TransferService::record_bytes_relayed(&state_for_recv, chunk_len);
                        TransferService::try_send_sender(
                            &state_for_recv,
                            &code_for_recv,
                            SenderEvent::Progress {
                                bytes_transferred: bytes_received,
                                total_bytes: expected_file_size.unwrap_or(bytes_received),
                            },
                        )
                        .await;
                        TransferService::try_send_receiver(
                            &state_for_recv,
                            &code_for_recv,
                            DownloadEvent::Progress {
                                bytes_transferred: bytes_received,
                                total_bytes: expected_file_size.unwrap_or(bytes_received),
                            },
                        )
                        .await;
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
