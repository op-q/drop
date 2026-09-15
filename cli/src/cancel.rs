//! Stopping a transfer on purpose, and telling the other side.
//!
//! Before this, Ctrl-C ran the termination handler, which restored the terminal
//! and exited. The peer found out from a dropped connection, and the relay
//! counted it as a failure. Now the first Ctrl-C (or SIGTERM) fires a
//! [`Cancel`]. Whatever the transfer is waiting on next, through a
//! [`Cancellable`] transport, sends the peer `cancel` and stops. A second signal,
//! or two seconds without the transfer stopping, exits at once as before, so a
//! stuck transfer can always be left.
//!
//! The wrapper is the whole mechanism on purpose. Every read and write already
//! goes through a [`Transport`], so wrapping the transport reaches every wait in
//! both directions without threading a token through each function that waits.
//! Nothing a transfer writes is abandoned half-way: a cancel is noticed between
//! frames, never during one.

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::transport::{Frame, Transport, TransportError};

/// How long a cancelling peer waits to tell the other side before giving up
/// on it. Telling is a courtesy; leaving is not negotiable.
const TELL_TIMEOUT: Duration = Duration::from_secs(1);

/// How long after a first interrupt the process waits for the transfer to stop
/// on its own before exiting anyway.
const GRACE: Duration = Duration::from_secs(2);

/// A request to stop, shared by whoever may make it and whoever must obey it.
#[derive(Clone, Default)]
pub struct Cancel {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    fired: AtomicBool,
    notify: Notify,
}

impl Cancel {
    /// Asks everything holding this to stop. Idempotent.
    pub fn fire(&self) {
        if !self.inner.fired.swap(true, Ordering::AcqRel) {
            self.inner.notify.notify_waiters();
        }
    }

    pub fn is_fired(&self) -> bool {
        self.inner.fired.load(Ordering::Acquire)
    }

    /// Resolves once [`Cancel::fire`] has been called, including before this
    /// was.
    pub async fn fired(&self) {
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            // Registered before the flag is read, so a `fire` between the
            // two cannot be missed.
            notified.as_mut().enable();

            if self.is_fired() {
                return;
            }

            notified.await;
        }
    }
}

/// Fires `cancel` on the first interrupt, and exits the process on a second, or
/// if the transfer has not stopped a short while after the first.
///
/// The exit path is the one the termination handlers always had, and it keeps
/// its order: the terminal first, since a raw terminal cannot be recovered from,
/// then spool files.
pub fn exit_on_second_interrupt(cancel: Cancel) {
    tokio::spawn(async move {
        crate::payload::wait_for_termination().await;
        cancel.fire();
        eprintln!();
        eprintln!("Cancelling. Press Ctrl-C again to quit at once.");

        tokio::select! {
            () = crate::payload::wait_for_termination() => {}
            () = tokio::time::sleep(GRACE) => {}
        }

        crate::ui::terminal::restore();
        crate::payload::remove_spool_files();
        std::process::exit(130);
    });
}

/// A transport that stops when a [`Cancel`] fires, and tells the peer first.
///
/// While the cancel has not fired, every call passes straight through. Once it
/// has, the next call sends `{"type":"cancel","reason":"user"}` at most once,
/// bounded by [`TELL_TIMEOUT`], closes, and fails with
/// [`TransportError::Cancelled`]. A wait already in progress, the common case,
/// is interrupted. That is safe because a receive must be cancel-safe (see
/// [`Transport::receive`]), and sends are not interrupted, only refused before
/// they start.
pub struct Cancellable<T> {
    inner: T,
    cancel: Cancel,
    told: bool,
}

impl<T: Transport + Send> Cancellable<T> {
    pub fn new(inner: T, cancel: Cancel) -> Self {
        Self {
            inner,
            cancel,
            told: false,
        }
    }

    async fn stop(&mut self) -> TransportError {
        if !self.told {
            self.told = true;
            let _ = tokio::time::timeout(
                TELL_TIMEOUT,
                self.inner
                    .send_control(json!({ "type": "cancel", "reason": "user" })),
            )
            .await;
            let _ = tokio::time::timeout(TELL_TIMEOUT, self.inner.close()).await;
        }

        TransportError::Cancelled
    }
}

impl<T: Transport + Send> Transport for Cancellable<T> {
    fn peers_enforce_one_guess(&self) -> bool {
        self.inner.peers_enforce_one_guess()
    }

    async fn await_peer(&mut self) -> Result<(), TransportError> {
        let cancel = self.cancel.clone();
        tokio::select! {
            biased;
            () = cancel.fired() => Err(self.stop().await),
            waited = self.inner.await_peer() => waited,
        }
    }

    async fn send_control(&mut self, frame: Value) -> Result<(), TransportError> {
        if self.cancel.is_fired() {
            return Err(self.stop().await);
        }

        self.inner.send_control(frame).await
    }

    async fn send_chunk(&mut self, chunk: Vec<u8>) -> Result<(), TransportError> {
        if self.cancel.is_fired() {
            return Err(self.stop().await);
        }

        self.inner.send_chunk(chunk).await
    }

    async fn receive(&mut self) -> Result<Option<Frame>, TransportError> {
        let cancel = self.cancel.clone();
        tokio::select! {
            biased;
            () = cancel.fired() => Err(self.stop().await),
            frame = self.inner.receive() => frame,
        }
    }

    async fn close(&mut self) {
        self.inner.close().await;
    }
}

/// How a transfer ended without happening, in the terms a program can act on.
///
/// Carried as an error so every path can return it with `?`, and matched by
/// the binary for its exit code: see `main.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The receiver said no, or did not answer in time. Exit code 3.
    Declined(String),
    /// The other side stopped the transfer. Exit code 4.
    PeerCancelled(String),
}

impl Ended {
    /// The process exit code for this ending.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Declined(_) => 3,
            Self::PeerCancelled(_) => 4,
        }
    }
}

impl fmt::Display for Ended {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Declined(message) | Self::PeerCancelled(message) => formatter.write_str(message),
        }
    }
}

impl Error for Ended {}

/// The exit code for a failed run: 3 declined, 4 cancelled by the peer, 130
/// cancelled here, 1 anything else.
pub fn exit_code_for(error: &(dyn Error + 'static)) -> u8 {
    if let Some(ended) = error.downcast_ref::<Ended>() {
        return ended.exit_code();
    }

    if matches!(
        error.downcast_ref::<TransportError>(),
        Some(TransportError::Cancelled)
    ) {
        return 130;
    }

    1
}

#[cfg(test)]
mod tests {
    use super::{Cancel, Cancellable, Ended, exit_code_for};
    use crate::transport::{Frame, Transport, TransportError, scripted::ScriptedTransport};
    use serde_json::json;
    use std::time::Duration;

    #[tokio::test]
    async fn firing_before_waiting_still_wakes_the_wait() {
        let cancel = Cancel::default();
        cancel.fire();

        tokio::time::timeout(Duration::from_secs(1), cancel.fired())
            .await
            .expect("an earlier fire must not be missed");
    }

    #[tokio::test]
    async fn a_wait_in_progress_is_interrupted_and_the_peer_is_told_once() {
        let cancel = Cancel::default();
        let mut transport = Cancellable::new(
            ScriptedTransport::saying(vec![]).held_open(),
            cancel.clone(),
        );

        let firing = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                cancel.fire();
            })
        };

        let received = tokio::time::timeout(Duration::from_secs(2), transport.receive())
            .await
            .expect("the cancel must interrupt a silent peer");
        firing.await.expect("fired");

        assert!(matches!(received, Err(TransportError::Cancelled)));

        // A second call after the first must not tell the peer again.
        let again = transport.send_control(json!({ "type": "complete" })).await;
        assert!(matches!(again, Err(TransportError::Cancelled)));

        let told = transport.inner.sent_control();
        assert_eq!(told.len(), 1, "the peer is told exactly once: {told:?}");
        assert_eq!(told[0]["type"], "cancel");
        assert_eq!(told[0]["reason"], "user");
    }

    #[tokio::test]
    async fn nothing_is_sent_after_a_cancel_except_the_cancel() {
        let cancel = Cancel::default();
        let mut transport = Cancellable::new(ScriptedTransport::saying(vec![]), cancel.clone());

        transport
            .send_chunk(vec![1, 2, 3])
            .await
            .expect("before the cancel, sends pass through");
        cancel.fire();

        let refused = transport.send_chunk(vec![4, 5, 6]).await;
        assert!(matches!(refused, Err(TransportError::Cancelled)));

        assert_eq!(transport.inner.sent_chunks().len(), 1);
    }

    #[tokio::test]
    async fn an_unfired_cancel_changes_nothing() {
        let mut transport = Cancellable::new(
            ScriptedTransport::saying(vec![json!({ "type": "meta" })]),
            Cancel::default(),
        );

        let Some(Frame::Control(frame)) = transport.receive().await.expect("a frame") else {
            panic!("the frame should pass through");
        };
        assert_eq!(frame["type"], "meta");
    }

    #[test]
    fn each_ending_has_its_own_exit_code() {
        let declined: Box<dyn std::error::Error + Send + Sync> =
            Box::new(Ended::Declined("no".into()));
        let peer: Box<dyn std::error::Error + Send + Sync> =
            Box::new(Ended::PeerCancelled("stopped".into()));
        let here: Box<dyn std::error::Error + Send + Sync> = Box::new(TransportError::Cancelled);
        let other: Box<dyn std::error::Error + Send + Sync> = "broken".into();

        assert_eq!(exit_code_for(declined.as_ref()), 3);
        assert_eq!(exit_code_for(peer.as_ref()), 4);
        assert_eq!(exit_code_for(here.as_ref()), 130);
        assert_eq!(exit_code_for(other.as_ref()), 1);
    }
}
