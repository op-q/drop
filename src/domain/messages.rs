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