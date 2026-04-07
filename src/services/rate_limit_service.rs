use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::Mutex;

use crate::{
    config::{
        CONNECTION_ATTEMPT_LIMIT_PER_MINUTE, MAX_WS_CONNECTIONS_PER_IP,
        SESSION_CREATION_LIMIT_PER_MINUTE,
    },
    errors::AppError,
};

#[derive(Clone, Default)]
pub struct RateLimitService {
    inner: Arc<Mutex<RateLimitState>>,
}

#[derive(Default)]
struct RateLimitState {
    session_creations: HashMap<IpAddr, VecDeque<Instant>>,
    connection_attempts: HashMap<IpAddr, VecDeque<Instant>>,
    active_ws_connections: HashMap<IpAddr, usize>,
}

impl RateLimitService {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn check_session_creation_limit(&self, ip: IpAddr) -> Result<(), AppError> {
        let mut state = self.inner.lock().await;
        Self::check_and_record(
            &mut state.session_creations,
            ip,
            SESSION_CREATION_LIMIT_PER_MINUTE,
            Duration::from_secs(60),
        )
        .map_err(|_| {
            AppError::TooManyRequests(
                "too many session creation requests from this IP; try again in a minute".into(),
            )
        })
    }

    pub async fn check_connection_attempt_limit(&self, ip: IpAddr) -> Result<(), AppError> {
        let mut state = self.inner.lock().await;
        Self::check_and_record(
            &mut state.connection_attempts,
            ip,
            CONNECTION_ATTEMPT_LIMIT_PER_MINUTE,
            Duration::from_secs(60),
        )
        .map_err(|_| {
            AppError::TooManyRequests(
                "too many WebSocket connection attempts from this IP; try again shortly".into(),
            )
        })
    }

    pub async fn try_acquire_ws_connection(&self, ip: IpAddr) -> Result<(), AppError> {
        let mut state = self.inner.lock().await;
        let count = state.active_ws_connections.entry(ip).or_insert(0);

        if *count >= MAX_WS_CONNECTIONS_PER_IP {
            return Err(AppError::TooManyRequests(format!(
                "too many active WebSocket connections from this IP (max {})",
                MAX_WS_CONNECTIONS_PER_IP
            )));
        }

        *count += 1;
        Ok(())
    }

    pub async fn release_ws_connection(&self, ip: IpAddr) {
        let mut state = self.inner.lock().await;

        if let Some(count) = state.active_ws_connections.get_mut(&ip) {
            if *count > 1 {
                *count -= 1;
            } else {
                state.active_ws_connections.remove(&ip);
            }
        }
    }

    fn check_and_record(
        buckets: &mut HashMap<IpAddr, VecDeque<Instant>>,
        ip: IpAddr,
        limit: usize,
        window: Duration,
    ) -> Result<(), ()> {
        let now = Instant::now();
        let bucket = buckets.entry(ip).or_default();

        while let Some(front) = bucket.front() {
            if now.duration_since(*front) >= window {
                bucket.pop_front();
            } else {
                break;
            }
        }

        if bucket.len() >= limit {
            return Err(());
        }

        bucket.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::RateLimitService;

    #[tokio::test]
    async fn blocks_session_creation_after_limit() {
        let service = RateLimitService::new();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        for _ in 0..10 {
            service
                .check_session_creation_limit(ip)
                .await
                .expect("expected request to be allowed");
        }

        assert!(service.check_session_creation_limit(ip).await.is_err());
    }

    #[tokio::test]
    async fn releases_websocket_slots() {
        let service = RateLimitService::new();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        for _ in 0..4 {
            service
                .try_acquire_ws_connection(ip)
                .await
                .expect("expected connection slot");
        }

        assert!(service.try_acquire_ws_connection(ip).await.is_err());

        service.release_ws_connection(ip).await;

        assert!(service.try_acquire_ws_connection(ip).await.is_ok());
    }
}
