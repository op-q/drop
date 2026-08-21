use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SenderMessage {
    /// One half of the SPAKE2 exchange, forwarded to the receiver untouched.
    ///
    /// The relay cannot use this. It carries no filename and no key, and the
    /// password that would make it meaningful never leaves either client.
    KeyExchange {
        message: String,
    },
    /// Describes the payload. Everything that used to be readable here —
    /// filename and MIME type — is now inside `metadata`, sealed under a key
    /// the relay does not have.
    Meta {
        version: u8,
        ciphertext_size: u64,
        metadata: String,
    },
    Complete,
    Cancel,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReceiverMessage {
    /// The receiver's half of the SPAKE2 exchange, forwarded to the sender.
    KeyExchange {
        message: String,
    },
    ChunkAck {
        bytes_received: u64,
    },
    Complete {
        bytes_received: u64,
    },
    Error,
}
