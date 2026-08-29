//! Checks that the relay and the client still agree on the wire contract.
//!
//! The relay cannot depend on the client crate, so a few protocol constants
//! are necessarily written down twice. These tests are what stop the two
//! copies drifting apart silently — a drift that would show up as transfers
//! failing in the field rather than as a build error.

/// `api::config::ENVELOPE_VERSION` and `drop_cli::crypto::ENVELOPE_VERSION`
/// are the same number in two crates. The relay refuses a version it does not
/// recognise, so if these disagree, every transfer fails at `meta`.
#[test]
fn envelope_version_matches_the_client() {
    assert_eq!(
        api::config::ENVELOPE_VERSION,
        drop_cli::crypto::ENVELOPE_VERSION,
        "the relay and the client disagree on the envelope version"
    );
}

/// The relay bounds the opaque fields it forwards. A sealed metadata blob has
/// to fit inside that bound with room to spare, or ordinary transfers with
/// long filenames would be refused by the relay rather than by anything
/// meaningful.
#[test]
fn a_sealed_metadata_blob_fits_well_inside_the_relays_opaque_field_limit() {
    use drop_cli::crypto::{Handshake, Metadata, TransferCode, seal_metadata, to_hex};

    let code = TransferCode::parse("7F2A91-abandon-ability-able").unwrap();
    let (sender, sender_message) = Handshake::start(&code);
    let (receiver, receiver_message) = Handshake::start(&code);
    let keys = sender.finish(&receiver_message).unwrap();
    drop(receiver.finish(&sender_message));

    let blob = seal_metadata(
        &keys,
        1024,
        &Metadata {
            filename: "a".repeat(255),
            mime_type: "application/octet-stream".to_string(),
            plaintext_size: 1024,
        },
    )
    .unwrap();

    assert!(
        to_hex(&blob).len() < api::config::MAX_OPAQUE_FIELD_BYTES,
        "a metadata blob with a maximum-length filename exceeds the relay's limit"
    );
}

/// `drop_crypto::CHUNK_PLAINTEXT_BYTES` and `drop_cli::payload::CHUNK_BYTES`
/// are the same number in two crates. The sealer's chunk indices only line up
/// with what the sender actually reads off disk while these two agree.
///
/// This assertion used to live inside the envelope's own tests, where both
/// values were reachable as modules of one crate. The envelope became a
/// separate crate so the browser client could compile it to WebAssembly, which
/// put the payload reader out of its reach — so the guard moves up to the
/// layer that can still see both, rather than being dropped.
#[test]
fn the_envelope_chunk_size_matches_the_payload_reader() {
    assert_eq!(
        drop_cli::crypto::CHUNK_PLAINTEXT_BYTES,
        drop_cli::payload::CHUNK_BYTES as u64,
        "the envelope and the payload reader disagree on the chunk size"
    );
}
