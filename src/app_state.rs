use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{
    errors::AppError, services::rate_limit_service::RateLimitService, store::InMemorySessionStore,
    telemetry::metrics::AppMetrics,
};

#[derive(Clone)]
pub struct AppState {
    pub sessions: InMemorySessionStore,
    pub rate_limiter: RateLimitService,
    pub metrics: AppMetrics,
    accepting_connections: Arc<AtomicBool>,
}

impl AppState {
    pub fn new(
        sessions: InMemorySessionStore,
        rate_limiter: RateLimitService,
        metrics: AppMetrics,
    ) -> Self {
        Self {
            sessions,
            rate_limiter,
            metrics,
            accepting_connections: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn ensure_accepting_connections(&self) -> Result<(), AppError> {
        if self.is_accepting_connections() {
            Ok(())
        } else {
            Err(AppError::ServiceUnavailable(
                "server is draining for shutdown; retry on another instance".into(),
            ))
        }
    }

    pub fn is_accepting_connections(&self) -> bool {
        self.accepting_connections.load(Ordering::Acquire)
    }

    pub fn begin_draining(&self) {
        self.accepting_connections.store(false, Ordering::Release);
    }
}
