//! SPAKE2 key agreement, seeded by the transfer code.
//!
//! The key is never transmitted and is never derived from anything a relay
//! holds. Both peers run the exchange, and a peer that does not know the code
//! finishes with a different key — which fails at the first sealed frame
//! rather than at the handshake, because SPAKE2 does not reveal whether the
//! password matched.
//!
//! **A failed handshake must consume the session.** The security of a 44-bit
//! code rests entirely on an attacker getting one guess. That guarantee is a
//! property of how the transports use this module, not something this module
//! can enforce on its own; see `docs/decisions.md` entry 7.

use hkdf::Hkdf;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{CryptoError, code::TransferCode};

/// Domain separator prefix, so a Drop handshake can never be replayed into
/// another protocol that happens to use SPAKE2 with the same group. The
/// session's nameplate is appended to it — see `identity_for`.
const IDENTITY_PREFIX: &str = "drop/v1/transfer/";

/// Binds the handshake to one session.
///
/// The nameplate is public, so this adds no secrecy. What it adds is that a
/// relay running two sessions cannot splice a handshake message from one into
/// the other: the identities differ, so the exchange simply fails to agree.
fn identity_for(code: &TransferCode) -> Vec<u8> {
    format!("{IDENTITY_PREFIX}{}", code.nameplate()).into_bytes()
}

const CHUNK_KEY_INFO: &[u8] = b"drop/v1/chunk";
const META_KEY_INFO: &[u8] = b"drop/v1/meta";
const SALT_INFO: &[u8] = b"drop/v1/salt";

/// The nonce prefix length. The remaining 8 bytes of the 96-bit nonce are the
/// chunk counter.
pub const SALT_BYTES: usize = 4;

/// Keys for one transfer, derived from the agreed secret.
///
/// Zeroized on drop. That is not a complete defence — Rust moves values around
/// and a copy may survive in a buffer we do not own — but leaving key material
/// in freed memory when clearing it costs one derive is not defensible either.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SessionKeys {
    pub(crate) chunk_key: [u8; 32],
    pub(crate) meta_key: [u8; 32],
    pub(crate) salt: [u8; SALT_BYTES],
}

/// One side of an in-progress key agreement.
pub struct Handshake {
    state: Spake2<Ed25519Group>,
}

impl Handshake {
    /// Begins agreement, returning the message to hand to the peer.
    ///
    /// Symmetric mode is used because either peer may connect first and the
    /// protocol has no natural A/B assignment; giving one out would be an
    /// extra thing for both transports to agree on and get wrong.
    pub fn start(code: &TransferCode) -> (Self, Vec<u8>) {
        let (state, outbound) = Spake2::<Ed25519Group>::start_symmetric(
            &Password::new(code.as_password()),
            &Identity::new(&identity_for(code)),
        );

        (Self { state }, outbound)
    }

    /// Completes agreement against the peer's message.
    ///
    /// Succeeding here does **not** mean the peer knew the code. It means the
    /// message was well formed. A wrong code produces a well-formed message
    /// and a different key, and is detected when the first sealed frame fails
    /// to open.
    pub fn finish(self, peer_message: &[u8]) -> Result<SessionKeys, CryptoError> {
        let shared = self
            .state
            .finish(peer_message)
            .map_err(|error| CryptoError::Handshake(error.to_string()))?;

        let mut keys = SessionKeys {
            chunk_key: [0u8; 32],
            meta_key: [0u8; 32],
            salt: [0u8; SALT_BYTES],
        };

        let derived = Hkdf::<Sha256>::new(None, &shared);
        for (info, output) in [
            (CHUNK_KEY_INFO, &mut keys.chunk_key[..]),
            (META_KEY_INFO, &mut keys.meta_key[..]),
            (SALT_INFO, &mut keys.salt[..]),
        ] {
            derived
                .expand(info, output)
                .map_err(|error| CryptoError::Handshake(error.to_string()))?;
        }

        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agree(sender_code: &str, receiver_code: &str) -> (SessionKeys, SessionKeys) {
        let sender_code = TransferCode::parse(sender_code).unwrap();
        let receiver_code = TransferCode::parse(receiver_code).unwrap();

        let (sender, sender_message) = Handshake::start(&sender_code);
        let (receiver, receiver_message) = Handshake::start(&receiver_code);

        (
            sender.finish(&receiver_message).unwrap(),
            receiver.finish(&sender_message).unwrap(),
        )
    }

    #[test]
    fn matching_codes_agree_on_every_key() {
        let code = "7F2A91-abandon-ability-able";
        let (sender, receiver) = agree(code, code);

        assert_eq!(sender.chunk_key, receiver.chunk_key);
        assert_eq!(sender.meta_key, receiver.meta_key);
        assert_eq!(sender.salt, receiver.salt);
    }

    #[test]
    fn a_wrong_code_completes_the_handshake_but_agrees_on_nothing() {
        let (sender, receiver) = agree(
            "7F2A91-abandon-ability-able",
            "7F2A91-abandon-ability-above",
        );

        // Finishing succeeded on both sides: SPAKE2 does not reveal a password
        // mismatch. The divergence is what the first sealed frame detects.
        assert_ne!(sender.chunk_key, receiver.chunk_key);
        assert_ne!(sender.meta_key, receiver.meta_key);
    }

    #[test]
    fn the_three_derived_secrets_differ_from_each_other() {
        let code = "7F2A91-abandon-ability-able";
        let (keys, _) = agree(code, code);

        assert_ne!(keys.chunk_key, keys.meta_key);
        assert_ne!(&keys.chunk_key[..SALT_BYTES], &keys.salt[..]);
    }

    #[test]
    fn two_sessions_on_the_same_code_derive_different_keys() {
        let code = "7F2A91-abandon-ability-able";
        let (first, _) = agree(code, code);
        let (second, _) = agree(code, code);

        // SPAKE2 draws fresh randomness per run, so the same code twice is two
        // unrelated sessions. This is what makes a per-transfer nonce counter
        // starting at zero safe.
        assert_ne!(first.chunk_key, second.chunk_key);
    }

    /// Same words, different session. The relay cannot move a handshake
    /// message from one transfer into another.
    #[test]
    fn the_same_words_under_different_nameplates_do_not_agree() {
        let first = TransferCode::parse("7F2A91-abandon-ability-able").unwrap();
        let second = TransferCode::parse("B4C3D2-abandon-ability-able").unwrap();

        let (sender, sender_message) = Handshake::start(&first);
        let (receiver, receiver_message) = Handshake::start(&second);

        let sender_keys = sender.finish(&receiver_message).unwrap();
        let receiver_keys = receiver.finish(&sender_message).unwrap();

        assert_ne!(sender_keys.chunk_key, receiver_keys.chunk_key);
    }

    #[test]
    fn a_malformed_peer_message_is_rejected() {
        let code = TransferCode::parse("7F2A91-abandon-ability-able").unwrap();
        let (handshake, _) = Handshake::start(&code);

        assert!(matches!(
            handshake.finish(b"not a spake2 message"),
            Err(CryptoError::Handshake(_))
        ));
    }
}
