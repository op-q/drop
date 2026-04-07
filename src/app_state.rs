use crate::{
    services::rate_limit_service::RateLimitService, store::InMemorySessionStore,
    telemetry::metrics::AppMetrics,
};

#[derive(Clone)]
pub struct AppState {
    pub sessions: InMemorySessionStore,
    pub rate_limiter: RateLimitService,
    pub metrics: AppMetrics,
}
