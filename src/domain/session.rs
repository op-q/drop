use std::time::Instant;

use tokio::sync::mpsc;

use crate::services::relay_budget::RelayReservation;

#[derive(Clone)]
pub struct Session {
    pub filename: String,
    pub file_size: u64,
    pub created_at: Instant,
    pub last_activity: Instant,
    pub sender_tx: Option<mpsc::Sender<SenderEvent>>,
    pub download_tx: Option<mpsc::Sender<DownloadEvent>>,
    pub sender_connected: bool,
    pub receiver_connected: bool,
    pub bytes_relayed: u64,
    pub receiver_acknowledged_bytes: u64,
    pub sender_finished: bool,
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
    Meta {
        filename: String,
        file_size: u64,
        mime_type: String,
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
