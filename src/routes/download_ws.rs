use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::info;

use crate::{
    app_state::AppState,
    domain::session::{DownloadEvent, SenderEvent},
};

pub async fn download_ws(
    ws: WebSocketUpgrade,
    Path(code): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, code, state))
}

async fn handle_socket(socket: WebSocket, code: String, state: AppState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (download_tx, mut download_rx) = mpsc::channel::<DownloadEvent>(32);

    let sender_tx = {
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

        if session.download_tx.is_some() {
            let _ = ws_sender
                .send(Message::Text(
                    r#"{"type":"error","message":"session already claimed"}"#.into(),
                ))
                .await;
            let _ = ws_sender.close().await;
            return;
        }

        session.download_tx = Some(download_tx);
        session.receiver_connected = true;

        session.sender_tx.clone()
    };

    info!("receiver connected for code {}", code);

    if let Some(sender_tx) = sender_tx {
        let _ = sender_tx.send(SenderEvent::Status("receiver_connected")).await;
    }

    let _ = ws_sender
        .send(Message::Text(
            serde_json::json!({
                "type": "status",
                "status": "waiting_for_sender"
            })
            .to_string(),
        ))
        .await;

    let send_task = tokio::spawn(async move {
        while let Some(event) = download_rx.recv().await {
            match event {
                DownloadEvent::Status(status) => {
                    let msg = serde_json::json!({
                        "type": "status",
                        "status": status
                    });

                    if ws_sender.send(Message::Text(msg.to_string())).await.is_err() {
                        break;
                    }
                }
                DownloadEvent::Meta {
                    filename,
                    file_size,
                    mime_type,
                } => {
                    let msg = serde_json::json!({
                        "type": "meta",
                        "filename": filename,
                        "file_size": file_size,
                        "mime_type": mime_type
                    });

                    if ws_sender.send(Message::Text(msg.to_string())).await.is_err() {
                        break;
                    }
                }
                DownloadEvent::Chunk(chunk) => {
                    if ws_sender.send(Message::Binary(chunk)).await.is_err() {
                        break;
                    }
                }
                DownloadEvent::Complete => {
                    let _ = ws_sender
                        .send(Message::Text(
                            serde_json::json!({
                                "type": "complete"
                            })
                            .to_string(),
                        ))
                        .await;
                    break;
                }
                DownloadEvent::Error(message) => {
                    let _ = ws_sender
                        .send(Message::Text(
                            serde_json::json!({
                                "type": "error",
                                "message": message
                            })
                            .to_string(),
                        ))
                        .await;
                    break;
                }
            }
        }
    });

    let state_for_recv = state.clone();
    let code_for_recv = code.clone();

    let recv_task = tokio::spawn(async move {
        while let Some(result) = ws_receiver.next().await {
            match result {
                Ok(Message::Close(_)) => {
                    let sender_tx = {
                        let sessions = state_for_recv.sessions.lock().await;
                        sessions
                            .get(&code_for_recv)
                            .and_then(|session| session.sender_tx.clone())
                    };

                    if let Some(sender_tx) = sender_tx {
                        let _ = sender_tx
                            .send(SenderEvent::Error("receiver disconnected".into()))
                            .await;
                    }

                    {
                        let mut sessions = state_for_recv.sessions.lock().await;
                        sessions.remove(&code_for_recv);
                    }

                    break;
                }
                Ok(_) => {}
                Err(_) => {
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

    info!("receiver disconnected for code {}", code);
}