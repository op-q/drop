//! Where two peers meet, derived from the public half of the code.
//!
//! A direct transfer has no server to introduce the peers, so they have to
//! compute a meeting point from what they both already hold. That is the
//! nameplate: the sender publishes its address under a keypair derived from it,
//! and the receiver derives the same keypair and looks the address up. Nothing
//! is exchanged out of band beyond the code the human already carries.
//!
//! **This key is derived from the nameplate and never from the words.** The
//! nameplate is public and carries nothing. The words are the key-exchange
//! password, and a record keyed on them would be a public artifact an attacker
//! could grind 33 bits against offline — which is precisely the failure the
//! code was split in two to prevent. See `docs/decisions.md` entries 7 and 10.
//!
//! # What this key is not
//!
//! It is **not a secret**. Its input is public, so anyone who knows or guesses
//! a nameplate derives the identical keypair, including its private half.
//! Everything that follows from that is deliberate and has to be designed for
//! rather than assumed away:
//!
//! - Anyone may publish a record under it, so a resolved address is **not
//!   proof of who published it**. Authentication is the key exchange's job, and
//!   only the key exchange's job.
//! - Anyone may resolve it, so a published record discloses the sender's
//!   address to anyone willing to enumerate a 24-bit space. That is the
//!   address-disclosure weakness recorded in entry 10, and it has no
//!   counterpart in the relay design.
//!
//! What stops a wrong peer getting bytes is SPAKE2. What stops it *grinding*
//! for them is that a failed attempt must consume the transfer — a property the
//! relay used to provide by refusing a second claim on a session, and which a
//! serverless transfer has to enforce for itself.

use hkdf::Hkdf;
use sha2::Sha256;

use crate::code::TransferCode;

/// Domain separator. Distinct from every info string in `handshake.rs`, so
/// this output can never coincide with key material that seals a payload.
const RENDEZVOUS_INFO: &[u8] = b"drop/v1/rendezvous";

/// Length of the derived seed, which is an ed25519 secret scalar's worth.
pub const RENDEZVOUS_SECRET_BYTES: usize = 32;

/// Derives the seed both peers use to find each other.
///
/// Takes the whole code rather than a bare string so the nameplate is already
/// normalised — a receiver who typed the nameplate in lowercase must arrive at
/// the same meeting point as the sender who printed it in upper. Only
/// [`TransferCode::nameplate`] is read; the words are never touched, and
/// `the_meeting_point_ignores_the_words` holds that open.
///
/// Returned as raw bytes rather than as a signing key, because the crate that
/// turns these into a published record is the transport's business and this
/// crate has no opinion about which one it is.
pub fn rendezvous_secret(code: &TransferCode) -> [u8; RENDEZVOUS_SECRET_BYTES] {
    let mut secret = [0u8; RENDEZVOUS_SECRET_BYTES];

    Hkdf::<Sha256>::new(None, code.nameplate().as_bytes())
        .expand(RENDEZVOUS_INFO, &mut secret)
        .expect("32 bytes is far below HKDF-SHA256's 8160-byte output limit");

    secret
}

#[cfg(test)]
mod tests {
    use super::{RENDEZVOUS_SECRET_BYTES, rendezvous_secret};
    use crate::code::TransferCode;

    fn code(text: &str) -> TransferCode {
        TransferCode::parse(text).expect("a well-formed code")
    }

    /// The load-bearing test in this module.
    ///
    /// The meeting point is published where anyone can read it. If the words
    /// influenced it, that published record would be an offline oracle for the
    /// key-exchange password, and 33 bits would fall in seconds. Two codes
    /// sharing a nameplate must therefore meet in exactly the same place.
    #[test]
    fn the_meeting_point_ignores_the_words() {
        assert_eq!(
            rendezvous_secret(&code("7F2A91-abandon-ability-able")),
            rendezvous_secret(&code("7F2A91-zone-zoo-zebra")),
            "the words must not reach the meeting point"
        );
    }

    #[test]
    fn a_different_nameplate_is_a_different_meeting_point() {
        assert_ne!(
            rendezvous_secret(&code("7F2A91-abandon-ability-able")),
            rendezvous_secret(&code("4607F9-abandon-ability-able"))
        );
    }

    /// A receiver retypes the code by hand, and nameplates are printed
    /// uppercase. Normalising inside the derivation is what stops a lowercase
    /// retype from looking somewhere nobody published.
    #[test]
    fn a_retyped_nameplate_finds_the_same_place() {
        assert_eq!(
            rendezvous_secret(&code("4607f9-abandon-ability-able")),
            rendezvous_secret(&code("4607F9-abandon-ability-able"))
        );
    }

    #[test]
    fn the_same_code_always_derives_the_same_seed() {
        let first = rendezvous_secret(&code("7F2A91-abandon-ability-able"));
        let second = rendezvous_secret(&code("7F2A91-abandon-ability-able"));

        assert_eq!(first, second);
        assert_eq!(first.len(), RENDEZVOUS_SECRET_BYTES);
        assert_ne!(first, [0u8; RENDEZVOUS_SECRET_BYTES], "not the zero key");
    }
}
