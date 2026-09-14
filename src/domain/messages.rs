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
    /// The sender abandons the transfer. `reason` is one of a fixed set;
    /// see [`cancel_reason`].
    Cancel {
        #[serde(default)]
        reason: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReceiverMessage {
    /// The receiver's half of the SPAKE2 exchange, forwarded to the sender.
    KeyExchange {
        message: String,
    },
    /// The receiver opened the sealed metadata, and proves it with a value
    /// only a peer holding the same keys can produce. Forwarded to the sender
    /// untouched: the relay cannot check the proof, and does not need to.
    MetaOk {
        confirmation: String,
    },
    ChunkAck {
        bytes_received: u64,
    },
    Complete {
        bytes_received: u64,
    },
    Error,
    /// The receiver saw what was offered and agreed. Until this arrives the
    /// relay carries no chunk.
    Accept,
    /// The receiver saw what was offered and refused, or did not answer in
    /// time. A normal ending, not a failure.
    Decline {
        #[serde(default)]
        reason: Option<String>,
    },
    /// The receiver abandons the transfer.
    Cancel {
        #[serde(default)]
        reason: Option<String>,
    },
    /// Every byte has arrived and the receiver is closing the file or
    /// finishing an extraction, which for a large archive takes a while.
    Finishing,
}

/// Why a receiver declined, from the fixed set peers agree on.
///
/// Normalised here rather than forwarded as sent, so the relay never passes a
/// peer's own text to the other peer's terminal. Anything unrecognised is an
/// ordinary decline.
pub fn decline_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("timed_out") => "timed_out",
        _ => "declined",
    }
}

/// Why a peer cancelled, from the fixed set peers agree on. Anything
/// unrecognised is a person choosing to stop.
pub fn cancel_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("write_failed") => "write_failed",
        Some("read_failed") => "read_failed",
        Some("integrity") => "integrity",
        Some("too_large") => "too_large",
        _ => "user",
    }
}
