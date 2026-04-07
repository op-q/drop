use std::{net::IpAddr, time::Instant};

use tokio::sync::mpsc;
use tracing::info;
use uuid::Uuid;

use crate::{
    app_state::AppState,
    config::{MAX_CONCURRENT_SESSIONS, MAX_UPLOAD_SIZE_BYTES, MAX_UPLOAD_SIZE_LABEL},
    domain::session::{DownloadEvent, SenderEvent, Session},
    errors::AppError,
    services::cleanup_service::remove_expired_sessions,
};

pub struct SessionService;

pub enum SenderClaimResult {
    InvalidCode,
    AlreadyConnected,
    Accepted { receiver_connected: bool },
}

pub enum ReceiverClaimResult {
    InvalidCode,
    AlreadyClaimed,
    Accepted {
        sender_tx: Option<mpsc::Sender<SenderEvent>>,
    },
}

impl SessionService {
    pub async fn create_session(
        state: &AppState,
        client_ip: IpAddr,
        filename: String,
        file_size: u64,
    ) -> Result<String, AppError> {
        if filename.trim().is_empty() {
            return Err(AppError::BadRequest("filename is required".into()));
        }

        if file_size == 0 {
            return Err(AppError::BadRequest(
                "file size must be greater than zero".into(),
            ));
        }

        if file_size > MAX_UPLOAD_SIZE_BYTES {
            return Err(AppError::PayloadTooLarge(format!(
                "file size exceeds the {} upload limit ({} bytes max)",
                MAX_UPLOAD_SIZE_LABEL, MAX_UPLOAD_SIZE_BYTES
            )));
        }

        remove_expired_sessions(state).await;
        state
            .rate_limiter
            .check_session_creation_limit(client_ip)
            .await?;

        if state.sessions.len().await >= MAX_CONCURRENT_SESSIONS {
            return Err(AppError::ServiceUnavailable(format!(
                "too many active sessions; the server currently allows up to {} concurrent sessions",
                MAX_CONCURRENT_SESSIONS
            )));
        }

        let code = Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(6)
            .collect::<String>()
            .to_uppercase();

        state
            .sessions
            .insert(
                code.clone(),
                Session {
                    filename,
                    file_size,
                    created_at: Instant::now(),
                    sender_tx: None,
                    download_tx: None,
                    sender_connected: false,
                    receiver_connected: false,
                },
            )
            .await;

        state.metrics.record_session_created();
        info!(session_code = %code, file_size, client_ip = %client_ip, "session created");

        Ok(code)
    }

    pub async fn claim_sender(
        state: &AppState,
        code: &str,
        sender_tx: mpsc::Sender<SenderEvent>,
    ) -> SenderClaimResult {
        state
            .sessions
            .with_session_mut(code, move |session| {
                if session.sender_connected {
                    SenderClaimResult::AlreadyConnected
                } else {
                    let receiver_connected = session.receiver_connected;
                    session.sender_connected = true;
                    session.sender_tx = Some(sender_tx);
                    SenderClaimResult::Accepted { receiver_connected }
                }
            })
            .await
            .unwrap_or(SenderClaimResult::InvalidCode)
    }

    pub async fn claim_receiver(
        state: &AppState,
        code: &str,
        download_tx: mpsc::Sender<DownloadEvent>,
    ) -> ReceiverClaimResult {
        state
            .sessions
            .with_session_mut(code, move |session| {
                if session.download_tx.is_some() {
                    ReceiverClaimResult::AlreadyClaimed
                } else {
                    let sender_tx = session.sender_tx.clone();
                    session.download_tx = Some(download_tx);
                    session.receiver_connected = true;
                    ReceiverClaimResult::Accepted { sender_tx }
                }
            })
            .await
            .unwrap_or(ReceiverClaimResult::InvalidCode)
    }

    pub async fn session_file_size(state: &AppState, code: &str) -> Option<u64> {
        state
            .sessions
            .get(code)
            .await
            .map(|session| session.file_size)
    }

    pub async fn sender_tx(state: &AppState, code: &str) -> Option<mpsc::Sender<SenderEvent>> {
        state
            .sessions
            .get(code)
            .await
            .and_then(|session| session.sender_tx)
    }

    pub async fn download_tx(state: &AppState, code: &str) -> Option<mpsc::Sender<DownloadEvent>> {
        state
            .sessions
            .get(code)
            .await
            .and_then(|session| session.download_tx)
    }

    pub async fn receiver_connected(state: &AppState, code: &str) -> bool {
        state
            .sessions
            .get(code)
            .await
            .map(|session| session.receiver_connected)
            .unwrap_or(false)
    }

    pub async fn remove_session(state: &AppState, code: &str) -> Option<Session> {
        state.sessions.remove(code).await
    }
}
