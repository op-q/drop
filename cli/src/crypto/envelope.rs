//! Chunk framing: sealing plaintext into authenticated chunks and opening them
//! back in order.
//!
//! Integrity here has to cover more than the contents of each chunk. A relay
//! sits in the path and can drop, reorder, duplicate, or truncate the stream
//! without touching a single byte inside a chunk. Binding the chunk's position
//! and the total count into the additional authenticated data is what makes
//! all four of those fail the tag instead of producing plausible output.

use aes_gcm::{
    Aes256Gcm,
    aead::{Aead, KeyInit, Nonce, Payload},
};
use serde::{Deserialize, Serialize};

use crate::crypto::{CryptoError, handshake::SessionKeys};

/// Bumped when the framing changes in a way an older build cannot read. A
/// mismatch is a hard failure: a version that is negotiated downward is a
/// version a hostile relay can negotiate to plaintext.
pub const ENVELOPE_VERSION: u8 = 1;

/// AES-GCM authentication tag, appended to every sealed chunk.
pub const TAG_BYTES: u64 = 16;

/// Plaintext bytes per chunk, matching the relay's `RECOMMENDED_CHUNK_BYTES`
/// so a sealed chunk still fits inside the frame ceiling with room to spare.
pub const CHUNK_PLAINTEXT_BYTES: u64 = 1024 * 1024;

/// The nonce counter reserved for the metadata blob.
///
/// `u64::MAX` cannot collide with a chunk counter: the 4 GiB transfer limit
/// caps chunk indices at a few thousand, and `Sealer` refuses to exceed the
/// declared count regardless.
const METADATA_COUNTER: u64 = u64::MAX;

/// What the relay used to see in cleartext and now does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub filename: String,
    pub mime_type: String,
    /// Plaintext length. The relay only ever learns the sealed length, but the
    /// receiver needs this one: it drives progress, the decompression
    /// expansion guard, and the check that the file that landed is the size
    /// the sender meant to send.
    pub plaintext_size: u64,
}

/// Chunks needed for a payload of this many plaintext bytes.
pub fn total_chunks(plaintext_len: u64) -> u64 {
    plaintext_len.div_ceil(CHUNK_PLAINTEXT_BYTES)
}

/// Sealed length for a payload of this many plaintext bytes.
///
/// Deterministic, which is what lets a sender still declare an exact total
/// before the first byte moves — see `docs/decisions.md` entry 2. Compression
/// happens before encryption, so the input here is the compressed length.
pub fn ciphertext_len(plaintext_len: u64) -> u64 {
    plaintext_len + TAG_BYTES * total_chunks(plaintext_len)
}

fn nonce(salt: &[u8; 4], counter: u64) -> Nonce<Aes256Gcm> {
    let mut raw = [0u8; 12];
    raw[..4].copy_from_slice(salt);
    raw[4..].copy_from_slice(&counter.to_be_bytes());
    raw.into()
}

/// Binds the version, the chunk's position, and the total count.
fn chunk_aad(index: u64, total: u64) -> [u8; 17] {
    let mut aad = [0u8; 17];
    aad[0] = ENVELOPE_VERSION;
    aad[1..9].copy_from_slice(&index.to_be_bytes());
    aad[9..].copy_from_slice(&total.to_be_bytes());
    aad
}

/// The metadata blob binds the **sealed** size rather than the chunk count.
///
/// It cannot bind the chunk count: that is a function of the plaintext size,
/// which is inside the blob, and the receiver has to open the blob to learn
/// it. The sealed size is known to both ends before anything is opened —
/// the sender declares it and the relay forwards it — so binding it here also
/// means a relay that alters the declared size cannot have the metadata open.
fn metadata_aad(ciphertext_size: u64) -> [u8; 17] {
    chunk_aad(METADATA_COUNTER, ciphertext_size)
}

/// Seals the transfer details.
///
/// Free-standing rather than a method on [`Sealer`] so it mirrors
/// [`open_metadata`], which cannot be a method on [`Opener`]: the receiver has
/// to open this before it knows the plaintext size an [`Opener`] needs.
pub fn seal_metadata(
    keys: &SessionKeys,
    ciphertext_size: u64,
    metadata: &Metadata,
) -> Result<Vec<u8>, CryptoError> {
    let plaintext = serde_json::to_vec(metadata)
        .map_err(|error| CryptoError::MalformedMetadata(error.to_string()))?;

    Aes256Gcm::new(&keys.meta_key.into())
        .encrypt(
            &nonce(&keys.salt, METADATA_COUNTER),
            Payload {
                msg: &plaintext,
                aad: &metadata_aad(ciphertext_size),
            },
        )
        .map_err(|_| CryptoError::MetadataAuthentication)
}

/// Opens the transfer details.
///
/// This is where a mistyped code is caught, because it is the first sealed
/// thing a receiver opens — before any destination is created and before a
/// byte is written.
pub fn open_metadata(
    keys: &SessionKeys,
    ciphertext_size: u64,
    blob: &[u8],
) -> Result<Metadata, CryptoError> {
    let plaintext = Aes256Gcm::new(&keys.meta_key.into())
        .decrypt(
            &nonce(&keys.salt, METADATA_COUNTER),
            Payload {
                msg: blob,
                aad: &metadata_aad(ciphertext_size),
            },
        )
        .map_err(|_| CryptoError::MetadataAuthentication)?;

    serde_json::from_slice(&plaintext)
        .map_err(|error| CryptoError::MalformedMetadata(error.to_string()))
}

/// The sending half of one transfer.
pub struct Sealer {
    cipher: Aes256Gcm,
    salt: [u8; 4],
    total: u64,
    next: u64,
}

impl Sealer {
    pub fn new(keys: &SessionKeys, plaintext_len: u64) -> Self {
        Self {
            cipher: Aes256Gcm::new(&keys.chunk_key.into()),
            salt: keys.salt,
            total: total_chunks(plaintext_len),
            next: 0,
        }
    }

    /// Total chunks this transfer will produce.
    pub fn total_chunks(&self) -> u64 {
        self.total
    }

    /// Seals the next chunk. Chunks must be sealed in order, which the counter
    /// enforces rather than trusts.
    pub fn seal_chunk(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if self.next >= self.total {
            return Err(CryptoError::TooManyChunks {
                declared: self.total,
            });
        }

        let sealed = self
            .cipher
            .encrypt(
                &nonce(&self.salt, self.next),
                Payload {
                    msg: plaintext,
                    aad: &chunk_aad(self.next, self.total),
                },
            )
            .map_err(|_| CryptoError::ChunkAuthentication { index: self.next })?;

        self.next += 1;
        Ok(sealed)
    }
}

/// The receiving half of one transfer.
pub struct Opener {
    cipher: Aes256Gcm,
    salt: [u8; 4],
    total: u64,
    next: u64,
}

impl Opener {
    pub fn new(keys: &SessionKeys, plaintext_len: u64) -> Self {
        Self {
            cipher: Aes256Gcm::new(&keys.chunk_key.into()),
            salt: keys.salt,
            total: total_chunks(plaintext_len),
            next: 0,
        }
    }

    /// Opens the next chunk in sequence.
    ///
    /// A chunk that arrives out of order fails here rather than being
    /// buffered: the transports deliver in order, so a gap means the stream
    /// was tampered with, not that the network reordered anything.
    pub fn open_chunk(&mut self, sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if self.next >= self.total {
            return Err(CryptoError::TooManyChunks {
                declared: self.total,
            });
        }

        let plaintext = self
            .cipher
            .decrypt(
                &nonce(&self.salt, self.next),
                Payload {
                    msg: sealed,
                    aad: &chunk_aad(self.next, self.total),
                },
            )
            .map_err(|_| CryptoError::ChunkAuthentication { index: self.next })?;

        self.next += 1;
        Ok(plaintext)
    }

    /// Confirms every declared chunk arrived.
    ///
    /// Truncation is the one attack the per-chunk tag cannot catch on its own:
    /// every chunk that did arrive is perfectly authentic. Only counting them
    /// against the authenticated total detects a stream that simply stopped.
    pub fn finish(&self) -> Result<(), CryptoError> {
        if self.next != self.total {
            return Err(CryptoError::Truncated {
                expected: self.total,
                received: self.next,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{Handshake, TransferCode};

    fn keys_for(code: &str) -> (SessionKeys, SessionKeys) {
        let code = TransferCode::parse(code).unwrap();
        let (sender, sender_message) = Handshake::start(&code);
        let (receiver, receiver_message) = Handshake::start(&code);

        (
            sender.finish(&receiver_message).unwrap(),
            receiver.finish(&sender_message).unwrap(),
        )
    }

    fn matched_keys() -> (SessionKeys, SessionKeys) {
        keys_for("7F2A91-abandon-ability-able")
    }

    fn metadata() -> Metadata {
        Metadata {
            filename: "project.tar".to_string(),
            mime_type: "application/x-tar".to_string(),
            plaintext_size: 3000,
        }
    }

    #[test]
    fn a_payload_round_trips() {
        let (sender, receiver) = matched_keys();
        let payload = vec![0xABu8; 3000];

        let mut sealer = Sealer::new(&sender, payload.len() as u64);
        let mut opener = Opener::new(&receiver, payload.len() as u64);

        let sealed_size = ciphertext_len(payload.len() as u64);
        let blob = seal_metadata(&sender, sealed_size, &metadata()).unwrap();
        assert_eq!(
            open_metadata(&receiver, sealed_size, &blob).unwrap(),
            metadata()
        );

        let sealed = sealer.seal_chunk(&payload).unwrap();
        assert_eq!(opener.open_chunk(&sealed).unwrap(), payload);
        opener.finish().unwrap();
    }

    #[test]
    fn a_multi_chunk_payload_round_trips() {
        let (sender, receiver) = matched_keys();
        let plaintext_len = CHUNK_PLAINTEXT_BYTES * 2 + 512;

        let mut sealer = Sealer::new(&sender, plaintext_len);
        let mut opener = Opener::new(&receiver, plaintext_len);
        assert_eq!(sealer.total_chunks(), 3);

        let mut remaining = plaintext_len;
        while remaining > 0 {
            let take = remaining.min(CHUNK_PLAINTEXT_BYTES);
            let chunk = vec![0x5Au8; take as usize];
            let sealed = sealer.seal_chunk(&chunk).unwrap();
            assert_eq!(opener.open_chunk(&sealed).unwrap(), chunk);
            remaining -= take;
        }

        opener.finish().unwrap();
    }

    #[test]
    fn the_declared_ciphertext_length_matches_what_is_actually_sent() {
        let (sender, _) = matched_keys();

        // Includes a length that is not a chunk multiple, and one that is
        // exactly one chunk, because off-by-one here would be invisible until
        // a real transfer stalled a byte short.
        for plaintext_len in [
            1u64,
            512,
            CHUNK_PLAINTEXT_BYTES - 1,
            CHUNK_PLAINTEXT_BYTES,
            CHUNK_PLAINTEXT_BYTES + 1,
            CHUNK_PLAINTEXT_BYTES * 3 + 7,
        ] {
            let mut sealer = Sealer::new(&sender, plaintext_len);
            let mut produced = 0u64;
            let mut remaining = plaintext_len;

            while remaining > 0 {
                let take = remaining.min(CHUNK_PLAINTEXT_BYTES);
                produced += sealer.seal_chunk(&vec![0u8; take as usize]).unwrap().len() as u64;
                remaining -= take;
            }

            assert_eq!(
                produced,
                ciphertext_len(plaintext_len),
                "declared and produced lengths differ for {plaintext_len} plaintext bytes"
            );
        }
    }

    #[test]
    fn a_wrong_code_fails_on_the_metadata_before_anything_is_written() {
        let (sender, _) = keys_for("7F2A91-abandon-ability-able");
        let (_, receiver) = keys_for("7F2A91-abandon-ability-above");

        let blob = seal_metadata(&sender, 1040, &metadata()).unwrap();
        assert!(matches!(
            open_metadata(&receiver, 1040, &blob),
            Err(CryptoError::MetadataAuthentication)
        ));
    }

    /// A relay that rewrites the declared sealed size cannot have the
    /// metadata open, because that size is what the blob authenticates against.
    #[test]
    fn metadata_will_not_open_against_a_different_declared_size() {
        let (sender, receiver) = matched_keys();
        let blob = seal_metadata(&sender, 1040, &metadata()).unwrap();

        assert!(matches!(
            open_metadata(&receiver, 2080, &blob),
            Err(CryptoError::MetadataAuthentication)
        ));
    }

    #[test]
    fn a_tampered_chunk_is_rejected() {
        let (sender, receiver) = matched_keys();
        let mut sealer = Sealer::new(&sender, 1024);
        let mut opener = Opener::new(&receiver, 1024);

        let mut sealed = sealer.seal_chunk(&[9u8; 1024]).unwrap();
        sealed[10] ^= 0x01;

        assert!(matches!(
            opener.open_chunk(&sealed),
            Err(CryptoError::ChunkAuthentication { index: 0 })
        ));
    }

    #[test]
    fn reordered_chunks_are_rejected() {
        let (sender, receiver) = matched_keys();
        let plaintext_len = CHUNK_PLAINTEXT_BYTES * 2;

        let mut sealer = Sealer::new(&sender, plaintext_len);
        let mut opener = Opener::new(&receiver, plaintext_len);

        let first = sealer
            .seal_chunk(&vec![1u8; CHUNK_PLAINTEXT_BYTES as usize])
            .unwrap();
        let second = sealer
            .seal_chunk(&vec![2u8; CHUNK_PLAINTEXT_BYTES as usize])
            .unwrap();

        // Delivering the second chunk first must fail: its AAD carries index 1
        // while the opener is authenticating against index 0.
        assert!(matches!(
            opener.open_chunk(&second),
            Err(CryptoError::ChunkAuthentication { index: 0 })
        ));
        drop(first);
    }

    #[test]
    fn a_truncated_stream_is_detected_even_though_every_chunk_was_authentic() {
        let (sender, receiver) = matched_keys();
        let plaintext_len = CHUNK_PLAINTEXT_BYTES * 3;

        let mut sealer = Sealer::new(&sender, plaintext_len);
        let mut opener = Opener::new(&receiver, plaintext_len);

        for _ in 0..2 {
            let sealed = sealer
                .seal_chunk(&vec![3u8; CHUNK_PLAINTEXT_BYTES as usize])
                .unwrap();
            opener.open_chunk(&sealed).unwrap();
        }

        assert!(matches!(
            opener.finish(),
            Err(CryptoError::Truncated {
                expected: 3,
                received: 2
            })
        ));
    }

    #[test]
    fn a_duplicated_chunk_is_rejected() {
        let (sender, receiver) = matched_keys();
        let plaintext_len = CHUNK_PLAINTEXT_BYTES * 2;

        let mut sealer = Sealer::new(&sender, plaintext_len);
        let mut opener = Opener::new(&receiver, plaintext_len);

        let first = sealer
            .seal_chunk(&vec![4u8; CHUNK_PLAINTEXT_BYTES as usize])
            .unwrap();

        opener.open_chunk(&first).unwrap();
        assert!(matches!(
            opener.open_chunk(&first),
            Err(CryptoError::ChunkAuthentication { index: 1 })
        ));
    }

    #[test]
    fn the_sealer_refuses_to_exceed_the_declared_chunk_count() {
        let (sender, _) = matched_keys();
        let mut sealer = Sealer::new(&sender, 16);

        sealer.seal_chunk(&[0u8; 16]).unwrap();
        assert!(matches!(
            sealer.seal_chunk(&[0u8; 16]),
            Err(CryptoError::TooManyChunks { declared: 1 })
        ));
    }

    #[test]
    fn a_nonce_is_never_reused_within_a_session() {
        let (sender, _) = matched_keys();
        let plaintext_len = CHUNK_PLAINTEXT_BYTES * 8;
        let sealer = Sealer::new(&sender, plaintext_len);

        let mut seen: Vec<[u8; 12]> = (0..sealer.total_chunks())
            .map(|index| {
                let raw: Nonce<Aes256Gcm> = nonce(&sender.salt, index);
                raw.into()
            })
            .collect();
        seen.push(nonce(&sender.salt, METADATA_COUNTER).into());

        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(before, seen.len(), "a nonce was reused within one session");
    }

    #[test]
    fn metadata_and_chunk_nonces_cannot_collide() {
        // The metadata counter is only safe because the chunk counter can
        // never reach it. Hold that to the transfer limit rather than to a
        // comment: 4 GiB of 1 MiB chunks is nowhere near u64::MAX.
        let max_chunks = total_chunks(4 * 1024 * 1024 * 1024);
        assert!(max_chunks < METADATA_COUNTER);
    }

    /// The sealer's chunk indices only line up with what the sender actually
    /// reads off disk while these two agree. They live in different modules,
    /// so pin them together.
    #[test]
    fn the_envelope_chunk_size_matches_the_payload_reader() {
        assert_eq!(CHUNK_PLAINTEXT_BYTES, crate::payload::CHUNK_BYTES as u64);
    }

    #[test]
    fn chunk_counts_are_correct_at_the_boundaries() {
        assert_eq!(total_chunks(0), 0);
        assert_eq!(total_chunks(1), 1);
        assert_eq!(total_chunks(CHUNK_PLAINTEXT_BYTES), 1);
        assert_eq!(total_chunks(CHUNK_PLAINTEXT_BYTES + 1), 2);
    }
}
