use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use serde::Serialize;
use tracing::info;

use crate::app_state::AppState;

const METRICS_LOG_INTERVAL_SECS: u64 = 60;

#[derive(Clone, Default)]
pub struct AppMetrics {
    inner: Arc<MetricsInner>,
}

#[derive(Default)]
struct MetricsInner {
    active_sessions: AtomicUsize,
    active_ws_connections: AtomicUsize,
    total_sessions_created: AtomicU64,
    total_sessions_expired: AtomicU64,
    total_transfers_completed: AtomicU64,
    total_transfer_failures: AtomicU64,
    total_bytes_relayed: AtomicU64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct MetricsSnapshot {
    pub active_sessions: usize,
    pub active_ws_connections: usize,
    pub total_sessions_created: u64,
    pub total_sessions_expired: u64,
    pub total_transfers_completed: u64,
    pub total_transfer_failures: u64,
    pub total_bytes_relayed: u64,
}

impl AppMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_session_created(&self) {
        self.inner
            .total_sessions_created
            .fetch_add(1, Ordering::Relaxed);
        self.inner.active_sessions.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_session_expired(&self) {
        self.inner
            .total_sessions_expired
            .fetch_add(1, Ordering::Relaxed);
        decrement(&self.inner.active_sessions);
    }

    pub fn record_transfer_completed(&self) {
        self.inner
            .total_transfers_completed
            .fetch_add(1, Ordering::Relaxed);
        decrement(&self.inner.active_sessions);
    }

    pub fn record_transfer_failed(&self) {
        self.inner
            .total_transfer_failures
            .fetch_add(1, Ordering::Relaxed);
        decrement(&self.inner.active_sessions);
    }

    pub fn record_bytes_relayed(&self, bytes: u64) {
        self.inner
            .total_bytes_relayed
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_ws_connection_opened(&self) {
        self.inner
            .active_ws_connections
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_ws_connection_closed(&self) {
        decrement(&self.inner.active_ws_connections);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            active_sessions: self.inner.active_sessions.load(Ordering::Relaxed),
            active_ws_connections: self.inner.active_ws_connections.load(Ordering::Relaxed),
            total_sessions_created: self.inner.total_sessions_created.load(Ordering::Relaxed),
            total_sessions_expired: self.inner.total_sessions_expired.load(Ordering::Relaxed),
            total_transfers_completed: self.inner.total_transfers_completed.load(Ordering::Relaxed),
            total_transfer_failures: self.inner.total_transfer_failures.load(Ordering::Relaxed),
            total_bytes_relayed: self.inner.total_bytes_relayed.load(Ordering::Relaxed),
        }
    }
}

pub fn spawn_metrics_task(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(METRICS_LOG_INTERVAL_SECS));

        loop {
            interval.tick().await;
            let snapshot = state.metrics.snapshot();
            info!(
                active_sessions = snapshot.active_sessions,
                active_ws_connections = snapshot.active_ws_connections,
                total_sessions_created = snapshot.total_sessions_created,
                total_sessions_expired = snapshot.total_sessions_expired,
                total_transfers_completed = snapshot.total_transfers_completed,
                total_transfer_failures = snapshot.total_transfer_failures,
                total_bytes_relayed = snapshot.total_bytes_relayed,
                "application metrics snapshot"
            );
        }
    });
}

fn decrement(counter: &AtomicUsize) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_sub(1))
    });
}
