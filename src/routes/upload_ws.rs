use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::IntoResponse,
};
use tokio::sync::mpsc;
use tracing::{info, warn};
use futures_util::{SinkExt, StreamExt};

use crate::{
    app_state::AppState,
    domain::{
        messages::SenderMessage,
        session::{DownloadEvent, SenderEvent},
    },
};

pub async fn upload_ws(
    ws: WebSocketUpgrade,
    Path(code): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, code, state))
}

async fn handle_socket(socket: WebSocket, code: String, state: AppState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (sender_tx, mut sender_rx) = mpsc::channel::<SenderEvent>(32);

    {
        let mut sessions = state.sessions.lock().await;

        let Some(session) = sessions.get_mut(&code) else {
            let _ = ws_sender
                .send(Message::Text(
                    r#"{"type":"error","message":"invalid session code"}"#.into(),
                ))
                .await;
            let _ = ws_sender.close().await;
            return;
        };

        if session.sender_connected {
            let _ = ws_sender
                .send(Message::Text(
                    r#"{"type":"error","message":"sender already connected"}"#.into(),
                ))
                .await;
            let _ = ws_sender.close().await;
            return;
        }

        session.sender_connected = true;
        session.sender_tx = Some(sender_tx.clone());
    }

    info!("sender connected for code {}", code);

    let receiver_already_connected = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&code)
            .map(|s| s.receiver_connected)
            .unwrap_or(false)
    };

    if receiver_already_connected {
        let _ = sender_tx.send(SenderEvent::Status("receiver_connected")).await;
    } else {
        let _ = sender_tx.send(SenderEvent::Status("waiting_for_receiver")).await;
    }

    let send_task = tokio::spawn(async move {
        while let Some(event) = sender_rx.recv().await {
            match event {
                SenderEvent::Status(status) => {
                    let msg = serde_json::json!({
                        "type": "status",
                        "status": status
                    });

                    if ws_sender.send(Message::Text(msg.to_string())).await.is_err() {
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
    });

    let state_for_recv = state.clone();
    let code_for_recv = code.clone();

    let recv_task = tokio::spawn(async move {
        let mut received_meta = false;

        while let Some(result) = ws_receiver.next().await {
            match result {
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<SenderMessage>(&text) {
                        Ok(SenderMessage::Meta {
                            filename,
                            file_size,
                            mime_type,
                        }) => {
                            let receiver_connected = {
                                let sessions = state_for_recv.sessions.lock().await;
                                sessions
                                    .get(&code_for_recv)
                                    .map(|s| s.receiver_connected)
                                    .unwrap_or(false)
                            };

                            if !receiver_connected {
                                let tx = {
                                    let sessions = state_for_recv.sessions.lock().await;
                                    sessions
                                        .get(&code_for_recv)
                                        .and_then(|session| session.sender_tx.clone())
                                };

                                if let Some(tx) = tx {
                                    let _ = tx
                                        .send(SenderEvent::Error(
                                            "receiver is not connected".into(),
                                        ))
                                        .await;
                                }
                                break;
                            }

                            received_meta = true;

                            let tx = {
                                let sessions = state_for_recv.sessions.lock().await;
                                sessions
                                    .get(&code_for_recv)
                                    .and_then(|session| session.download_tx.clone())
                            };

                            if let Some(tx) = tx {
                                let _ = tx
                                    .send(DownloadEvent::Meta {
                                        filename,
                                        file_size,
                                        mime_type,
                                    })
                                    .await;
                            }

                            let sender_tx = {
                                let sessions = state_for_recv.sessions.lock().await;
                                sessions
                                    .get(&code_for_recv)
                                    .and_then(|session| session.sender_tx.clone())
                            };

                            if let Some(sender_tx) = sender_tx {
                                let _ = sender_tx.send(SenderEvent::Status("sending")).await;
                            }
                        }

                        Ok(SenderMessage::Complete) => {
                            let download_tx = {
                                let sessions = state_for_recv.sessions.lock().await;
                                sessions
                                    .get(&code_for_recv)
                                    .and_then(|session| session.download_tx.clone())
                            };

                            if let Some(download_tx) = download_tx {
                                let _ = download_tx.send(DownloadEvent::Complete).await;
                            }

                            let sender_tx = {
                                let sessions = state_for_recv.sessions.lock().await;
                                sessions
                                    .get(&code_for_recv)
                                    .and_then(|session| session.sender_tx.clone())
                            };

                            if let Some(sender_tx) = sender_tx {
                                let _ = sender_tx
                                    .send(SenderEvent::Status("transfer_complete"))
                                    .await;
                            }

                            {
                                let mut sessions = state_for_recv.sessions.lock().await;
                                sessions.remove(&code_for_recv);
                            }

                            break;
                        }

                        Ok(SenderMessage::Cancel) => {
                            let download_tx = {
                                let sessions = state_for_recv.sessions.lock().await;
                                sessions
                                    .get(&code_for_recv)
                                    .and_then(|session| session.download_tx.clone())
                            };

                            if let Some(download_tx) = download_tx {
                                let _ = download_tx
                                    .send(DownloadEvent::Error("sender cancelled".into()))
                                    .await;
                            }

                            let sender_tx = {
                                let sessions = state_for_recv.sessions.lock().await;
                                sessions
                                    .get(&code_for_recv)
                                    .and_then(|session| session.sender_tx.clone())
                            };

                            if let Some(sender_tx) = sender_tx {
                                let _ = sender_tx.send(SenderEvent::Status("cancelled")).await;
                            }

                            {
                                let mut sessions = state_for_recv.sessions.lock().await;
                                sessions.remove(&code_for_recv);
                            }

                            break;
                        }

                        Err(err) => {
                            warn!("invalid sender control message for {}: {}", code_for_recv, err);

                            let sender_tx = {
                                let sessions = state_for_recv.sessions.lock().await;
                                sessions
                                    .get(&code_for_recv)
                                    .and_then(|session| session.sender_tx.clone())
                            };

                            if let Some(sender_tx) = sender_tx {
                                let _ = sender_tx
                                    .send(SenderEvent::Error("invalid control message".into()))
                                    .await;
                            }
                        }
                    }
                }

                Ok(Message::Binary(bytes)) => {
                    if !received_meta {
                        let sender_tx = {
                            let sessions = state_for_recv.sessions.lock().await;
                            sessions
                                .get(&code_for_recv)
                                .and_then(|session| session.sender_tx.clone())
                        };

                        if let Some(sender_tx) = sender_tx {
                            let _ = sender_tx
                                .send(SenderEvent::Error(
                                    "send meta before binary chunks".into(),
                                ))
                                .await;
                        }

                        continue;
                    }

                    let download_tx = {
                        let sessions = state_for_recv.sessions.lock().await;
                        sessions
                            .get(&code_for_recv)
                            .and_then(|session| session.download_tx.clone())
                    };

                    if let Some(download_tx) = download_tx {
                        let _ = download_tx.send(DownloadEvent::Chunk(bytes.to_vec())).await;
                    }
                }

                Ok(Message::Close(_)) => {
                    let download_tx = {
                        let sessions = state_for_recv.sessions.lock().await;
                        sessions
                            .get(&code_for_recv)
                            .and_then(|session| session.download_tx.clone())
                    };

                    if let Some(download_tx) = download_tx {
                        let _ = download_tx
                            .send(DownloadEvent::Error("sender disconnected".into()))
                            .await;
                    }

                    {
                        let mut sessions = state_for_recv.sessions.lock().await;
                        sessions.remove(&code_for_recv);
                    }

                    break;
                }

                Ok(_) => {}

                Err(err) => {
                    warn!("sender socket error for {}: {}", code_for_recv, err);

                    let download_tx = {
                        let sessions = state_for_recv.sessions.lock().await;
                        sessions
                            .get(&code_for_recv)
                            .and_then(|session| session.download_tx.clone())
                    };

                    if let Some(download_tx) = download_tx {
                        let _ = download_tx
                            .send(DownloadEvent::Error("sender socket error".into()))
                            .await;
                    }

                    {
                        let mut sessions = state_for_recv.sessions.lock().await;
                        sessions.remove(&code_for_recv);
                    }

                    break;
                }
            }
        }
    });

    let _ = tokio::join!(send_task, recv_task);

    info!("upload socket closed for {}", code);
}