use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use tokio::sync::mpsc;

use crate::services::relay_budget::RelayReservation;

#[derive(Clone)]
pub struct Session {
    /// Ciphertext bytes. The relay bounds and accounts for what it relays, and
    /// what it relays is sealed — it has no notion of a plaintext size, and no
    /// filename to go with it.
    pub ciphertext_size: u64,
    pub created_at: Instant,
    pub last_activity: Instant,
    pub sender_tx: Option<mpsc::Sender<SenderEvent>>,
    pub download_tx: Option<mpsc::Sender<DownloadEvent>>,
    pub sender_connected: bool,
    pub receiver_connected: bool,
    pub bytes_relayed: u64,
    pub receiver_acknowledged_bytes: u64,
    pub sender_finished: bool,
    /// Whether the relay has forwarded the sender's `meta`. The receiver
    /// cannot accept a transfer that has not been described to it.
    pub meta_forwarded: bool,
    /// Whether the receiver has accepted. Until it has, the relay carries no
    /// chunk. Shared rather than copied, so the upload socket can check it per
    /// chunk without taking the session lock each time.
    pub receiver_accepted: Acceptance,
}

/// The receiver's consent, readable from both sockets without the session lock.
#[derive(Clone, Debug, Default)]
pub struct Acceptance(Arc<AtomicBool>);

impl Acceptance {
    pub fn given(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub fn give(&self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Session {
    /// A session as `POST /api/session/create` makes one: neither peer
    /// connected, nothing relayed, nothing described, nothing accepted.
    pub fn new(ciphertext_size: u64) -> Self {
        let now = Instant::now();

        Self {
            ciphertext_size,
            created_at: now,
            last_activity: now,
            sender_tx: None,
            download_tx: None,
            sender_connected: false,
            receiver_connected: false,
            bytes_relayed: 0,
            receiver_acknowledged_bytes: 0,
            sender_finished: false,
            meta_forwarded: false,
            receiver_accepted: Acceptance::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum SenderEvent {
    Status(&'static str),
    Progress {
        bytes_transferred: u64,
        total_bytes: u64,
    },
    Acknowledgement {
        bytes_received: u64,
    },
    /// The receiver's key-exchange message, forwarded verbatim.
    KeyExchange(String),
    /// The receiver's key confirmation, forwarded verbatim.
    MetaOk(String),
    /// The receiver accepted the transfer.
    Accepted,
    /// The receiver declined. Terminal for the sender's socket.
    Declined(&'static str),
    /// The receiver cancelled. Terminal for the sender's socket.
    Cancelled(&'static str),
    /// The receiver has every byte and is finishing up.
    Finishing,
    Error(String),
}

/// Not `Clone`: a [`DownloadEvent::Chunk`] carries the relay-budget
/// reservation covering its own bytes, and duplicating an event would
/// duplicate buffered data the budget had only accounted for once.
pub enum DownloadEvent {
    Status(&'static str),
    Progress {
        bytes_transferred: u64,
        total_bytes: u64,
    },
    /// The sender's key-exchange message, forwarded verbatim.
    KeyExchange(String),
    /// The sender cancelled. Terminal for the receiver's socket.
    Cancelled(&'static str),
    Meta {
        version: u8,
        ciphertext_size: u64,
        metadata: String,
    },
    Chunk {
        data: Vec<u8>,
        /// Released when the chunk has been written to the receiver socket, or
        /// when a dropped channel discards it.
        reservation: RelayReservation,
    },
    Complete,
    Error(String),
}
