use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SenderMessage {
    Meta {
        filename: String,
        file_size: u64,
        mime_type: String,
    },
    Complete,
    Cancel,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReceiverMessage {
    ChunkAck { bytes_received: u64 },
    Complete { bytes_received: u64 },
    Error,
}
