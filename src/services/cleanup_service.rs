use std::time::{Duration, Instant};

use tracing::info;

use crate::{
    app_state::AppState,
    config::{CLEANUP_INTERVAL_SECS, SESSION_TTL_SECS},
    domain::session::{DownloadEvent, SenderEvent, Session},
};

pub fn spawn_cleanup_task(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(CLEANUP_INTERVAL_SECS));

        loop {
            interval.tick().await;
            remove_expired_sessions(&state).await;
        }
    });
}

pub async fn remove_expired_sessions(state: &AppState) {
    let now = Instant::now();
    let expired = state
        .sessions
        .with_all_mut(|sessions| {
            let expired_codes = sessions
                .iter()
                .filter_map(|(code, session)| {
                    if is_session_expired(session, now) {
                        Some(code.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();

            expired_codes
                .into_iter()
                .filter_map(|code| {
                    sessions.remove(&code).map(|session| {
                        (code, session.sender_tx.clone(), session.download_tx.clone())
                    })
                })
                .collect::<Vec<_>>()
        })
        .await;

    for (code, sender_tx, download_tx) in expired {
        if let Some(sender_tx) = sender_tx {
            let _ = sender_tx
                .send(SenderEvent::Error("session expired".into()))
                .await;
        }

        if let Some(download_tx) = download_tx {
            let _ = download_tx
                .send(DownloadEvent::Error("session expired".into()))
                .await;
        }

        state.metrics.record_session_expired();
        info!("expired session cleaned up: {}", code);
    }
}

pub fn is_session_expired(session: &Session, now: Instant) -> bool {
    now.duration_since(session.created_at) >= Duration::from_secs(SESSION_TTL_SECS)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::domain::session::Session;

    use super::is_session_expired;

    #[test]
    fn detects_expired_sessions() {
        let session = Session {
            filename: "file.bin".into(),
            file_size: 1,
            created_at: Instant::now() - Duration::from_secs(301),
            sender_tx: None,
            download_tx: None,
            sender_connected: false,
            receiver_connected: false,
        };

        assert!(is_session_expired(&session, Instant::now()));
    }
}
