use std::time::Instant;

use tokio::sync::mpsc;

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

#[derive(Debug, Clone)]
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
    Chunk(Vec<u8>),
    Complete,
    Error(String),
}
