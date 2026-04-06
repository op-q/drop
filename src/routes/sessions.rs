use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app_state::AppState,
    domain::session::Session,
};

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    pub filename: String,
}

#[derive(Serialize)]
pub struct CreateSessionResponse {
    pub code: String,
}

pub async fn create_session(
    State(state): State<AppState>,
    Json(payload): Json<CreateSessionRequest>,
) -> Json<CreateSessionResponse> {
    let code = Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(6)
        .collect::<String>()
        .to_uppercase();

    let session = Session {
        filename: payload.filename,
        sender_tx: None,
        download_tx: None,
        sender_connected: false,
        receiver_connected: false,
    };

    state.sessions.lock().await.insert(code.clone(), session);

    Json(CreateSessionResponse { code })
}