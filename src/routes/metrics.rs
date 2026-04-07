use axum::{Json, extract::State};

use crate::{app_state::AppState, telemetry::metrics::MetricsSnapshot};

pub async fn metrics(State(state): State<AppState>) -> Json<MetricsSnapshot> {
    Json(state.metrics.snapshot())
}
