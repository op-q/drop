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
use tokio::time::{Duration, interval, timeout};
use tracing::{Instrument, info, warn};

use crate::{
    app_state::AppState,
    config::{
        DOWNLOAD_EVENT_CHANNEL_CAPACITY, RECEIVER_SEND_TIMEOUT_SECS, WS_HEARTBEAT_INTERVAL_SECS,
        WS_IDLE_TIMEOUT_SECS, WS_MAX_MESSAGE_BYTES, client_ip_from_request,
    },
    domain::{
        messages::ReceiverMessage,
        session::{DownloadEvent, SenderEvent},
    },
    errors::AppError,
    services::{
        cleanup_service::remove_expired_sessions,
        session_service::{ReceiverClaimResult, SessionService},
        transfer_service::TransferService,
    },
    telemetry::tracing::transfer_span,
    ws::protocol,
};

pub async fn download_ws(
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

async fn handle_socket(socket: WebSocket, code: String, state: AppState, client_ip: IpAddr) {
    let span = transfer_span("download", &code);
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (download_tx, mut download_rx) =
        mpsc::channel::<DownloadEvent>(DOWNLOAD_EVENT_CHANNEL_CAPACITY);

    let sender_tx = match SessionService::claim_receiver(&state, &code, download_tx).await {
        ReceiverClaimResult::InvalidCode => {
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
        ReceiverClaimResult::AlreadyClaimed => {
            let _ = ws_sender
                .send(Message::Text(
                    r#"{"type":"error","message":"session already claimed"}"#.into(),
                ))
                .await;
            let _ = ws_sender.close().await;
            state.rate_limiter.release_ws_connection(client_ip).await;
            state.metrics.record_ws_connection_closed();
            return;
        }
        ReceiverClaimResult::Accepted { sender_tx } => sender_tx,
    };

    info!(session_code = %code, client_ip = %client_ip, "receiver connected");

    if let Some(sender_tx) = sender_tx {
        let _ = sender_tx
            .send(SenderEvent::Status("receiver_connected"))
            .await;
    }

    protocol::log_download_event(&code, &DownloadEvent::Status("waiting_for_sender"));
    let _ = ws_sender
        .send(Message::Text(
            serde_json::json!({
                "type": "status",
                "status": "waiting_for_sender"
            })
            .to_string()
            .into(),
        ))
        .await;

    let state_for_send = state.clone();
    let code_for_send = code.clone();

    let send_task = tokio::spawn(
        async move {
            let mut total_bytes = None;
            let mut heartbeat = interval(Duration::from_secs(WS_HEARTBEAT_INTERVAL_SECS));
            let send_timeout = Duration::from_secs(RECEIVER_SEND_TIMEOUT_SECS);

            loop {
                tokio::select! {
                    _ = heartbeat.tick() => {
                        if ws_sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                            break;
                        }
                    }
                    maybe_event = download_rx.recv() => {
                        let Some(event) = maybe_event else {
                            let _ = ws_sender.send(Message::Close(None)).await;
                            break;
                        };

                        match event {
                            DownloadEvent::Status(status) => {
                                let msg = serde_json::json!({
                                    "type": "status",
                                    "status": status
                                });

                                if ws_sender
                                    .send(Message::Text(msg.to_string().into()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            DownloadEvent::Progress {
                                bytes_transferred,
                                total_bytes: total,
                            } => {
                                let msg = serde_json::json!({
                                    "type": "progress",
                                    "bytes_transferred": bytes_transferred,
                                    "total_bytes": total
                                });

                                if ws_sender
                                    .send(Message::Text(msg.to_string().into()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            DownloadEvent::Meta {
                                filename,
                                file_size,
                                mime_type,
                            } => {
                                total_bytes = Some(file_size);
                                let msg = serde_json::json!({
                                    "type": "meta",
                                    "filename": filename,
                                    "file_size": file_size,
                                    "mime_type": mime_type
                                });

                                if ws_sender
                                    .send(Message::Text(msg.to_string().into()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            DownloadEvent::Chunk { data, reservation } => {
                                let sent = timeout(
                                    send_timeout,
                                    ws_sender.send(Message::Binary(data.into())),
                                )
                                .await;

                                // The chunk is no longer buffered by the relay,
                                // so return its bytes to the shared budget
                                // before handling the outcome.
                                drop(reservation);

                                match sent {
                                    Ok(Ok(())) => {}
                                    Ok(Err(_)) => {
                                        TransferService::fail_session(
                                            &state_for_send,
                                            &code_for_send,
                                            Some("receiver disconnected"),
                                            None,
                                            "receiver websocket send failed",
                                        )
                                        .await;
                                        break;
                                    }
                                    Err(_) => {
                                        let error_message = match total_bytes {
                                            Some(total_bytes) if total_bytes > 0 => {
                                                "receiver is too slow; transfer timed out"
                                            }
                                            _ => "receiver is too slow",
                                        };

                                        TransferService::fail_session(
                                            &state_for_send,
                                            &code_for_send,
                                            Some(error_message),
                                            None,
                                            "receiver send timeout",
                                        )
                                        .await;
                                        break;
                                    }
                                }
                            }
                            DownloadEvent::Complete => {
                                let complete_message = Message::Text(
                                    serde_json::json!({
                                        "type": "complete"
                                    })
                                    .to_string()
                                    .into(),
                                );

                                match timeout(send_timeout, ws_sender.send(complete_message)).await {
                                    Ok(Ok(())) => {}
                                    _ => {
                                        TransferService::fail_session(
                                            &state_for_send,
                                            &code_for_send,
                                            Some("receiver is too slow; transfer timed out"),
                                            None,
                                            "receiver complete send timed out",
                                        )
                                        .await;
                                        break;
                                    }
                                }
                            }
                            DownloadEvent::Error(message) => {
                                let _ = ws_sender
                                    .send(Message::Text(
                                        serde_json::json!({
                                            "type": "error",
                                            "message": message
                                        })
                                        .to_string()
                                        .into(),
                                    ))
                                    .await;
                                let _ = ws_sender.send(Message::Close(None)).await;
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

            loop {
                let result = match timeout(idle_timeout, ws_receiver.next()).await {
                    Ok(Some(result)) => result,
                    Ok(None) => break,
                    Err(_) => {
                        TransferService::fail_session(
                            &state_for_recv,
                            &code_for_recv,
                            Some("receiver connection timed out"),
                            None,
                            "receiver websocket timed out",
                        )
                        .await;
                        break;
                    }
                };

                match result {
                    Ok(Message::Text(text)) => {
                        let message = match serde_json::from_str::<ReceiverMessage>(&text) {
                            Ok(message) => message,
                            Err(err) => {
                                warn!(
                                    "invalid receiver control message for {}: {}",
                                    code_for_recv, err
                                );
                                TransferService::fail_session(
                                    &state_for_recv,
                                    &code_for_recv,
                                    Some("receiver sent an invalid control message"),
                                    Some("invalid control message"),
                                    "invalid receiver control message",
                                )
                                .await;
                                break;
                            }
                        };

                        match message {
                            ReceiverMessage::ChunkAck { bytes_received } => {
                                if !SessionService::acknowledge_receiver_bytes(
                                    &state_for_recv,
                                    &code_for_recv,
                                    bytes_received,
                                )
                                .await
                                {
                                    TransferService::fail_session(
                                        &state_for_recv,
                                        &code_for_recv,
                                        Some("receiver acknowledgement was invalid"),
                                        Some("transfer acknowledgement was invalid"),
                                        "invalid receiver chunk acknowledgement",
                                    )
                                    .await;
                                    break;
                                }

                                TransferService::send_sender(
                                    &state_for_recv,
                                    &code_for_recv,
                                    SenderEvent::Acknowledgement { bytes_received },
                                )
                                .await;
                            }
                            ReceiverMessage::Complete { bytes_received } => {
                                if !SessionService::confirm_receiver_complete(
                                    &state_for_recv,
                                    &code_for_recv,
                                    bytes_received,
                                )
                                .await
                                {
                                    TransferService::fail_session(
                                        &state_for_recv,
                                        &code_for_recv,
                                        Some("receiver completion was invalid"),
                                        Some("transfer completion was invalid"),
                                        "invalid receiver completion acknowledgement",
                                    )
                                    .await;
                                    break;
                                }

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
                            ReceiverMessage::Error => {
                                TransferService::fail_session(
                                    &state_for_recv,
                                    &code_for_recv,
                                    Some("receiver could not save the file"),
                                    None,
                                    "receiver reported a file write error",
                                )
                                .await;
                                break;
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        TransferService::fail_session(
                            &state_for_recv,
                            &code_for_recv,
                            Some("receiver disconnected"),
                            None,
                            "receiver closed websocket",
                        )
                        .await;
                        break;
                    }
                    Ok(_) => {}
                    Err(err) => {
                        warn!("receiver socket error for {}: {}", code_for_recv, err);
                        TransferService::fail_session(
                            &state_for_recv,
                            &code_for_recv,
                            Some("receiver disconnected"),
                            None,
                            "receiver websocket error",
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
            "download session ended without an explicit terminal event"
        );
    }

    state.rate_limiter.release_ws_connection(client_ip).await;
    state.metrics.record_ws_connection_closed();
    info!(session_code = %code, client_ip = %client_ip, "receiver socket closed");
}
