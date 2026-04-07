use tracing::{info, warn};

use crate::{
    app_state::AppState,
    domain::session::{DownloadEvent, SenderEvent},
    services::session_service::SessionService,
    ws::protocol,
};

pub struct TransferService;

impl TransferService {
    pub async fn send_sender(state: &AppState, code: &str, event: SenderEvent) {
        protocol::log_sender_event(code, &event);
        if let Some(sender_tx) = SessionService::sender_tx(state, code).await {
            let _ = sender_tx.send(event).await;
        }
    }

    pub async fn send_receiver(state: &AppState, code: &str, event: DownloadEvent) {
        protocol::log_download_event(code, &event);
        if let Some(download_tx) = SessionService::download_tx(state, code).await {
            let _ = download_tx.send(event).await;
        }
    }

    pub async fn try_send_sender(state: &AppState, code: &str, event: SenderEvent) {
        protocol::log_sender_event(code, &event);
        if let Some(sender_tx) = SessionService::sender_tx(state, code).await {
            let _ = sender_tx.try_send(event);
        }
    }

    pub async fn try_send_receiver(state: &AppState, code: &str, event: DownloadEvent) {
        protocol::log_download_event(code, &event);
        if let Some(download_tx) = SessionService::download_tx(state, code).await {
            let _ = download_tx.try_send(event);
        }
    }

    pub async fn fail_session(
        state: &AppState,
        code: &str,
        sender_message: Option<&str>,
        receiver_message: Option<&str>,
        reason: &str,
    ) {
        if let Some(message) = sender_message {
            Self::send_sender(state, code, SenderEvent::Error(message.into())).await;
        }

        if let Some(message) = receiver_message {
            Self::send_receiver(state, code, DownloadEvent::Error(message.into())).await;
        }

        if SessionService::remove_session(state, code).await.is_some() {
            state.metrics.record_transfer_failed();
            warn!(session_code = %code, reason, "transfer session failed");
        }
    }

    pub async fn complete_session(state: &AppState, code: &str, bytes_transferred: u64) {
        if SessionService::remove_session(state, code).await.is_some() {
            state.metrics.record_transfer_completed();
            info!(
                session_code = %code,
                bytes_transferred,
                "transfer session completed"
            );
        }
    }

    pub async fn expire_session(state: &AppState, code: &str) {
        if SessionService::remove_session(state, code).await.is_some() {
            state.metrics.record_session_expired();
            info!(session_code = %code, "transfer session expired");
        }
    }

    pub fn record_bytes_relayed(state: &AppState, bytes: u64) {
        state.metrics.record_bytes_relayed(bytes);
    }
}
