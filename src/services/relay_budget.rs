use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::config::{RELAY_BUDGET_BYTES, WS_MAX_MESSAGE_BYTES};

/// A server-wide ceiling on how many bytes of relayed file data may be held in
/// memory at once.
///
/// Every chunk buffered between the sender socket and the receiver socket holds
/// a [`RelayReservation`] for its own length. The reservation returns its bytes
/// to the pool when it is dropped, so a chunk that is written out, a channel
/// that is discarded when a session ends, and a task that unwinds all release
/// the same way. Nothing has to remember to hand the bytes back.
/// The production budget must admit at least one maximum-size frame, otherwise
/// a chunk of that size could never be relayed. Checked at compile time so a
/// future tuning change cannot introduce a stall that only shows up in
/// production.
const _: () = assert!(RELAY_BUDGET_BYTES >= WS_MAX_MESSAGE_BYTES);

#[derive(Clone)]
pub struct RelayBudget {
    permits: Arc<Semaphore>,
    capacity: usize,
}

/// Proof that `bytes` of relay budget are held. Dropping it releases them.
pub struct RelayReservation {
    permits: Arc<Semaphore>,
    bytes: usize,
}

impl RelayBudget {
    pub fn new() -> Self {
        Self::with_capacity(RELAY_BUDGET_BYTES)
    }

    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(bytes)),
            capacity: bytes,
        }
    }

    /// Waits until `bytes` of budget are free and takes them.
    ///
    /// Returns `None` only if the budget could never satisfy the request, which
    /// callers must treat as a rejected chunk rather than as backpressure.
    pub async fn reserve(&self, bytes: usize) -> Option<RelayReservation> {
        // A request larger than the whole pool would otherwise wait on capacity
        // that can never exist, hanging the transfer instead of failing it.
        if bytes > self.capacity {
            return None;
        }

        let requested = u32::try_from(bytes).ok()?;

        self.permits.acquire_many(requested).await.ok()?.forget();

        Some(RelayReservation {
            permits: Arc::clone(&self.permits),
            bytes,
        })
    }

    pub fn available_bytes(&self) -> usize {
        self.permits.available_permits()
    }
}

impl Default for RelayBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl RelayReservation {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Drop for RelayReservation {
    fn drop(&mut self) {
        self.permits.add_permits(self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use tokio::time::{Duration, timeout};

    use super::RelayBudget;

    #[tokio::test]
    async fn returns_bytes_to_the_pool_when_a_reservation_is_dropped() {
        let budget = RelayBudget::with_capacity(4096);

        let reservation = budget.reserve(3072).await.expect("first reservation");
        assert_eq!(budget.available_bytes(), 1024);

        drop(reservation);
        assert_eq!(budget.available_bytes(), 4096);
    }

    #[tokio::test]
    async fn makes_a_waiting_reservation_pause_until_capacity_returns() {
        let budget = RelayBudget::with_capacity(4096);
        let held = budget.reserve(4096).await.expect("full reservation");

        assert!(
            timeout(Duration::from_millis(50), budget.reserve(1024))
                .await
                .is_err(),
            "a reservation must wait while the pool is empty"
        );

        drop(held);
        let granted = timeout(Duration::from_millis(50), budget.reserve(1024))
            .await
            .expect("reservation should be granted once capacity returns");
        assert!(granted.is_some());
    }

    #[tokio::test]
    async fn rejects_a_request_larger_than_the_whole_pool_instead_of_hanging() {
        let budget = RelayBudget::with_capacity(4096);

        let rejected = timeout(Duration::from_millis(50), budget.reserve(4097))
            .await
            .expect("an impossible request must resolve rather than wait");

        assert!(rejected.is_none());
        assert_eq!(budget.available_bytes(), 4096);
    }

    #[tokio::test]
    async fn shares_one_ceiling_across_many_concurrent_sessions() {
        let budget = RelayBudget::with_capacity(8192);

        let mut held = Vec::new();
        for _ in 0..8 {
            held.push(budget.reserve(1024).await.expect("reservation"));
        }

        assert_eq!(budget.available_bytes(), 0);
        assert!(
            timeout(Duration::from_millis(50), budget.reserve(1))
                .await
                .is_err(),
            "the ceiling is global, not per session"
        );

        held.clear();
        assert_eq!(budget.available_bytes(), 8192);
    }
}
