//! The encryption envelope: transfer codes, key agreement, and chunk sealing.
//!
//! This module is deliberately transport-independent. It takes bytes and
//! returns bytes and knows nothing about sockets, because the same envelope
//! has to ride both the WebSocket relay and the peer-to-peer QUIC transport in
//! `docs/plans/peer-to-peer-transport-plan-2026-08-20.md`. That property is
//! what lets a relay be untrusted rather than removed.
//!
//! The design and its reasoning are in `docs/decisions.md` entry 7.

pub mod code;
pub mod envelope;
pub mod handshake;
mod wordlist;

use std::fmt;

pub use code::{CodeError, TransferCode};
pub use envelope::{
    CHUNK_PLAINTEXT_BYTES, ENVELOPE_VERSION, Metadata, Opener, Sealer, TAG_BYTES, ciphertext_len,
    open_metadata, seal_metadata, total_chunks,
};
pub use handshake::{Handshake, SessionKeys};

/// Renders opaque bytes for a JSON control frame.
///
/// The relay forwards these without understanding them, so the encoding only
/// has to be something both clients agree on and JSON can carry. Hex is chosen
/// over base64 for having no dependency and no variant confusion; the values
/// are well under a kilobyte, so the doubling costs nothing that matters.
pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Parses what [`to_hex`] produced, from a peer that may be hostile.
pub fn from_hex(text: &str) -> Result<Vec<u8>, CryptoError> {
    if text.len() % 2 != 0 {
        return Err(CryptoError::MalformedMetadata(
            "an encoded field had an odd length".into(),
        ));
    }

    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(|_| {
                CryptoError::MalformedMetadata("an encoded field was not text".into())
            })?;
            u8::from_str_radix(text, 16).map_err(|_| {
                CryptoError::MalformedMetadata("an encoded field was not hexadecimal".into())
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        assert_eq!(from_hex(&to_hex(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn hex_rejects_malformed_input() {
        assert!(from_hex("abc").is_err());
        assert!(from_hex("zz").is_err());
    }
}

/// Every way the envelope can refuse to produce plaintext.
///
/// These are kept distinct because the user-facing advice differs sharply: a
/// wrong code is a typo to re-enter, a failed chunk is a reason to distrust the
/// path, and a version mismatch is a reason to upgrade.
#[derive(Debug)]
pub enum CryptoError {
    /// The peer's key-agreement message was malformed or rejected.
    Handshake(String),
    /// A chunk did not authenticate. The overwhelmingly common cause is a
    /// mistyped code, because a wrong code yields a wrong key; the alternative
    /// is that something in the path altered the bytes.
    ChunkAuthentication { index: u64 },
    /// The metadata blob did not authenticate. Same causes, but this is the
    /// first thing opened, so it is where a wrong code is normally caught.
    MetadataAuthentication,
    /// The stream ended before every declared chunk arrived.
    Truncated { expected: u64, received: u64 },
    /// The sender tried to seal more chunks than it declared.
    TooManyChunks { declared: u64 },
    /// The peer speaks an envelope version this build does not.
    UnsupportedVersion { found: u8 },
    /// Metadata was authentic but not the shape we expect.
    MalformedMetadata(String),
}

impl fmt::Display for CryptoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Handshake(detail) => {
                write!(formatter, "key agreement failed: {detail}")
            }
            Self::ChunkAuthentication { index } => write!(
                formatter,
                "chunk {index} failed authentication — the transfer code may be wrong, \
                 or the data was altered in transit"
            ),
            Self::MetadataAuthentication => write!(
                formatter,
                "could not decrypt the transfer details — check the code and try again"
            ),
            Self::Truncated { expected, received } => write!(
                formatter,
                "the transfer ended early: {received} of {expected} chunks arrived"
            ),
            Self::TooManyChunks { declared } => write!(
                formatter,
                "tried to send more than the {declared} chunks declared for this transfer"
            ),
            Self::UnsupportedVersion { found } => write!(
                formatter,
                "the other end is using envelope version {found}, which this build \
                 does not support — upgrade drop on both computers"
            ),
            Self::MalformedMetadata(detail) => {
                write!(formatter, "the transfer details were unreadable: {detail}")
            }
        }
    }
}

impl std::error::Error for CryptoError {}
