use tokio::sync::mpsc;

#[derive(Clone)]
pub struct Session {
    pub filename: String,
    pub sender_tx: Option<mpsc::Sender<SenderEvent>>,
    pub download_tx: Option<mpsc::Sender<DownloadEvent>>,
    pub sender_connected: bool,
    pub receiver_connected: bool,
}

#[derive(Debug, Clone)]
pub enum SenderEvent {
    Status(&'static str),
    Error(String),
}

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Status(&'static str),
    Meta {
        filename: String,
        file_size: u64,
        mime_type: String,
    },
    Chunk(Vec<u8>),
    Complete,
    Error(String),
}