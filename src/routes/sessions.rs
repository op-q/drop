use std::net::SocketAddr;

use axum::{
    Json,
    extract::{ConnectInfo, State},
};
use serde::{Deserialize, Serialize};

use crate::{app_state::AppState, errors::AppError, services::session_service::SessionService};

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    pub filename: String,
    pub file_size: u64,
}

#[derive(Serialize)]
pub struct CreateSessionResponse {
    pub code: String,
}

pub async fn create_session(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, AppError> {
    let code =
        SessionService::create_session(&state, addr.ip(), payload.filename, payload.file_size)
            .await?;

    Ok(Json(CreateSessionResponse { code }))
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, time::Instant};

    use axum::extract::{ConnectInfo, State};

    use crate::{
        app_state::AppState,
        config::{MAX_CONCURRENT_SESSIONS, MAX_UPLOAD_SIZE_BYTES},
        domain::session::Session,
        services::rate_limit_service::RateLimitService,
        store::InMemorySessionStore,
        telemetry::metrics::AppMetrics,
    };

    use super::{CreateSessionRequest, create_session};

    #[tokio::test]
    async fn rejects_uploads_over_limit() {
        let state = AppState {
            sessions: InMemorySessionStore::new(),
            rate_limiter: RateLimitService::new(),
            metrics: AppMetrics::new(),
        };

        let result = create_session(
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))),
            State(state),
            axum::Json(CreateSessionRequest {
                filename: "too-big.bin".into(),
                file_size: MAX_UPLOAD_SIZE_BYTES + 1,
            }),
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn creates_session_when_file_size_is_within_limit() {
        let state = AppState {
            sessions: InMemorySessionStore::new(),
            rate_limiter: RateLimitService::new(),
            metrics: AppMetrics::new(),
        };

        let result = create_session(
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))),
            State(state.clone()),
            axum::Json(CreateSessionRequest {
                filename: "ok.bin".into(),
                file_size: MAX_UPLOAD_SIZE_BYTES,
            }),
        )
        .await;

        let response = result.expect("expected session creation to succeed");
        let stored = state
            .sessions
            .get(&response.code)
            .await
            .expect("expected stored session to exist");

        assert_eq!(stored.file_size, MAX_UPLOAD_SIZE_BYTES);
        assert_eq!(stored.filename, "ok.bin");
    }

    #[tokio::test]
    async fn rejects_when_session_capacity_is_full() {
        let sessions = InMemorySessionStore::new();
        for index in 0..MAX_CONCURRENT_SESSIONS {
            sessions
                .insert(
                    format!("CODE{:03}", index),
                    Session {
                        filename: "ok.bin".into(),
                        file_size: 1,
                        created_at: Instant::now(),
                        sender_tx: None,
                        download_tx: None,
                        sender_connected: false,
                        receiver_connected: false,
                    },
                )
                .await;
        }

        let state = AppState {
            sessions,
            rate_limiter: RateLimitService::new(),
            metrics: AppMetrics::new(),
        };

        let result = create_session(
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4321))),
            State(state),
            axum::Json(CreateSessionRequest {
                filename: "full.bin".into(),
                file_size: 1,
            }),
        )
        .await;

        assert!(result.is_err());
    }
}
