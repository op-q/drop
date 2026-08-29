use std::net::SocketAddr;

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};

use crate::{
    app_state::AppState, config::client_ip_from_request, errors::AppError,
    services::session_service::SessionService,
};

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    /// Sealed size, not plaintext size. The relay bounds what crosses it, and
    /// what crosses it is ciphertext; it has no way to learn the original.
    pub ciphertext_size: u64,
}

#[derive(Serialize)]
pub struct CreateSessionResponse {
    pub code: String,
}

pub async fn create_session(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, AppError> {
    state.ensure_accepting_connections()?;
    let client_ip = client_ip_from_request(addr.ip(), &headers);
    let code = SessionService::create_session(&state, client_ip, payload.ciphertext_size).await?;

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
        let state = AppState::new(
            InMemorySessionStore::new(),
            RateLimitService::new(),
            AppMetrics::new(),
        );

        let result = create_session(
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))),
            axum::http::HeaderMap::new(),
            State(state),
            axum::Json(CreateSessionRequest {
                ciphertext_size: MAX_UPLOAD_SIZE_BYTES + 1,
            }),
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn creates_session_when_ciphertext_size_is_within_limit() {
        let state = AppState::new(
            InMemorySessionStore::new(),
            RateLimitService::new(),
            AppMetrics::new(),
        );

        let result = create_session(
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))),
            axum::http::HeaderMap::new(),
            State(state.clone()),
            axum::Json(CreateSessionRequest {
                ciphertext_size: MAX_UPLOAD_SIZE_BYTES,
            }),
        )
        .await;

        let response = result.expect("expected session creation to succeed");
        let stored = state
            .sessions
            .get(&response.code)
            .await
            .expect("expected stored session to exist");

        assert_eq!(stored.ciphertext_size, MAX_UPLOAD_SIZE_BYTES);
    }

    #[tokio::test]
    async fn rejects_when_session_capacity_is_full() {
        let sessions = InMemorySessionStore::new();
        for index in 0..MAX_CONCURRENT_SESSIONS {
            sessions
                .insert(
                    format!("CODE{:03}", index),
                    Session {
                        ciphertext_size: 1,
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

        let state = AppState::new(sessions, RateLimitService::new(), AppMetrics::new());

        let result = create_session(
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4321))),
            axum::http::HeaderMap::new(),
            State(state),
            axum::Json(CreateSessionRequest { ciphertext_size: 1 }),
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rejects_new_sessions_while_draining() {
        let state = AppState::new(
            InMemorySessionStore::new(),
            RateLimitService::new(),
            AppMetrics::new(),
        );
        state.begin_draining();

        let result = create_session(
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))),
            axum::http::HeaderMap::new(),
            State(state),
            axum::Json(CreateSessionRequest { ciphertext_size: 1 }),
        )
        .await;

        assert!(result.is_err());
    }
}
