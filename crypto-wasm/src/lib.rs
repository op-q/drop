//! WebAssembly bindings for the Drop encryption envelope.
//!
//! There is no cryptography in this file. Every operation forwards to
//! `drop_crypto`, so the browser runs the same SPAKE2 transcript, the same
//! chunk framing, the same HKDF info strings, and the same wordlist as the
//! CLI. The alternative was a second implementation in TypeScript, which would
//! have made every future change to the envelope a byte-for-byte
//! compatibility exercise across two languages. See `docs/decisions.md`
//! entry 11.
//!
//! **This does not improve what a browser transfer can claim.** The page
//! fetches this module from the same origin that serves the JavaScript, so a
//! server willing to serve modified client code can serve a modified envelope
//! just as easily. `docs/decisions.md` entry 7 draws that line, and compiling
//! the Rust to wasm does not move it.

use drop_crypto::{CHUNK_PLAINTEXT_BYTES, ENVELOPE_VERSION, MAX_TRANSFER_BYTES, TAG_BYTES};
use wasm_bindgen::prelude::*;

/// Converts a size that arrived from JavaScript.
///
/// Sizes cross this boundary as `f64` rather than `u64`. wasm-bindgen maps
/// `u64` to `BigInt`, which would put a conversion at every call site in the
/// client and buy nothing: the relay refuses transfers above 4 GiB, and every
/// integer below 2^53 is exact in an `f64`. `File.size`, `ArrayBuffer`
/// lengths, and the WebSocket byte accounting are already JavaScript numbers,
/// so this keeps one numeric type across the whole client.
///
/// The validation is not decoration. `as u64` on a negative or fractional
/// float is a silent saturating cast in Rust, and a size that arrives wrong
/// would surface much later as a chunk-count mismatch.
fn size_from_js(value: f64, what: &str) -> Result<u64, JsError> {
    if !value.is_finite() || value.fract() != 0.0 || value < 0.0 {
        return Err(JsError::new(&format!(
            "{what} must be a whole, non-negative number of bytes"
        )));
    }

    if value > MAX_TRANSFER_BYTES as f64 {
        return Err(JsError::new(&format!(
            "{what} is larger than the {MAX_TRANSFER_BYTES} byte transfer limit"
        )));
    }

    Ok(value as u64)
}

fn to_js_error(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
}

/// The envelope version this build speaks.
///
/// A mismatch is a hard failure on both sides. Exposed as a function because
/// wasm-bindgen cannot export a constant.
#[wasm_bindgen(js_name = envelopeVersion)]
pub fn envelope_version() -> u8 {
    ENVELOPE_VERSION
}

/// Plaintext bytes per chunk. The client reads the file in slices of this size.
#[wasm_bindgen(js_name = chunkPlaintextBytes)]
pub fn chunk_plaintext_bytes() -> f64 {
    CHUNK_PLAINTEXT_BYTES as f64
}

/// The per-chunk authentication tag overhead.
///
/// The client needs this to keep the two byte scales apart: the relay meters
/// sealed bytes, the progress bar counts plaintext, and they differ by exactly
/// one tag per chunk.
#[wasm_bindgen(js_name = tagBytes)]
pub fn tag_bytes() -> f64 {
    TAG_BYTES as f64
}

/// The sealed length of a payload, known before the first byte moves.
#[wasm_bindgen(js_name = ciphertextLen)]
pub fn ciphertext_len(plaintext_size: f64) -> Result<f64, JsError> {
    let plaintext_size = size_from_js(plaintext_size, "the payload size")?;
    Ok(drop_crypto::ciphertext_len(plaintext_size) as f64)
}

/// A transfer code: a relay-visible nameplate and a secret that never leaves.
#[wasm_bindgen]
pub struct TransferCode {
    inner: drop_crypto::TransferCode,
}

#[wasm_bindgen]
impl TransferCode {
    /// Draws fresh secret words for a nameplate the relay just allocated.
    ///
    /// The nameplate is the only half the relay is told. The words are
    /// generated here, in the client, and are never sent anywhere.
    #[wasm_bindgen(js_name = generateFor)]
    pub fn generate_for(nameplate: &str) -> Result<TransferCode, JsError> {
        drop_crypto::TransferCode::generate_for(nameplate)
            .map(|inner| Self { inner })
            .map_err(to_js_error)
    }

    /// Parses a code a person typed, which is the hostile-input case.
    ///
    /// Accepts the separators and casing people actually produce; a code that
    /// cannot be parsed fails here, before the relay is contacted, with a
    /// message naming what was wrong with it.
    pub fn parse(input: &str) -> Result<TransferCode, JsError> {
        drop_crypto::TransferCode::parse(input)
            .map(|inner| Self { inner })
            .map_err(to_js_error)
    }

    /// The routing half, and the only half that may be sent to the relay.
    #[wasm_bindgen(getter)]
    pub fn nameplate(&self) -> String {
        self.inner.nameplate().to_string()
    }

    /// The whole code, for showing to the person who will read it aloud.
    #[wasm_bindgen(js_name = toString)]
    pub fn shareable(&self) -> String {
        self.inner.to_string()
    }
}

/// One side of an in-progress key agreement.
#[wasm_bindgen]
pub struct Handshake {
    inner: Option<drop_crypto::Handshake>,
    message: String,
}

#[wasm_bindgen]
impl Handshake {
    /// Begins agreement. The returned message is hex, ready for a JSON frame.
    pub fn start(code: &TransferCode) -> Handshake {
        let (inner, outbound) = drop_crypto::Handshake::start(&code.inner);

        Self {
            inner: Some(inner),
            message: drop_crypto::to_hex(&outbound),
        }
    }

    /// The message to hand the peer through the relay.
    #[wasm_bindgen(getter)]
    pub fn message(&self) -> String {
        self.message.clone()
    }

    /// Completes agreement against the peer's message.
    ///
    /// Succeeding here does **not** mean the peer knew the code — SPAKE2 does
    /// not reveal a password mismatch. A wrong code produces a well-formed
    /// message and a different key, and is caught when the sealed metadata
    /// fails to open.
    ///
    /// Takes `self` by value, so the JavaScript handle is invalidated: a
    /// handshake is single-use, and running one twice would reuse the secret
    /// scalar.
    pub fn finish(mut self, peer_message: &str) -> Result<SessionKeys, JsError> {
        let inner = self
            .inner
            .take()
            .ok_or_else(|| JsError::new("this handshake has already been completed"))?;

        let peer = drop_crypto::from_hex(peer_message).map_err(to_js_error)?;

        inner
            .finish(&peer)
            .map(|inner| SessionKeys { inner })
            .map_err(to_js_error)
    }
}

/// The derived keys for one transfer.
///
/// Deliberately opaque: there is no accessor, so the key material stays inside
/// the wasm module's linear memory rather than becoming a JavaScript array the
/// garbage collector will copy around and never clear.
#[wasm_bindgen]
pub struct SessionKeys {
    inner: drop_crypto::SessionKeys,
}

/// The transfer details the relay no longer sees.
#[wasm_bindgen]
pub struct Metadata {
    inner: drop_crypto::Metadata,
}

#[wasm_bindgen]
impl Metadata {
    #[wasm_bindgen(constructor)]
    pub fn new(
        filename: String,
        mime_type: String,
        plaintext_size: f64,
    ) -> Result<Metadata, JsError> {
        let plaintext_size = size_from_js(plaintext_size, "the payload size")?;

        Ok(Self {
            inner: drop_crypto::Metadata {
                filename,
                mime_type,
                plaintext_size,
            },
        })
    }

    #[wasm_bindgen(getter)]
    pub fn filename(&self) -> String {
        self.inner.filename.clone()
    }

    #[wasm_bindgen(getter, js_name = mimeType)]
    pub fn mime_type(&self) -> String {
        self.inner.mime_type.clone()
    }

    #[wasm_bindgen(getter, js_name = plaintextSize)]
    pub fn plaintext_size(&self) -> f64 {
        self.inner.plaintext_size as f64
    }
}

/// Seals the transfer details, returning hex for the `meta` frame.
#[wasm_bindgen(js_name = sealMetadata)]
pub fn seal_metadata(
    keys: &SessionKeys,
    ciphertext_size: f64,
    metadata: &Metadata,
) -> Result<String, JsError> {
    let ciphertext_size = size_from_js(ciphertext_size, "the sealed size")?;

    drop_crypto::seal_metadata(&keys.inner, ciphertext_size, &metadata.inner)
        .map(|blob| drop_crypto::to_hex(&blob))
        .map_err(to_js_error)
}

/// Opens the transfer details.
///
/// This is where a mistyped code is caught, before a download is started and
/// before anything is written.
#[wasm_bindgen(js_name = openMetadata)]
pub fn open_metadata(
    keys: &SessionKeys,
    ciphertext_size: f64,
    blob: &str,
) -> Result<Metadata, JsError> {
    let ciphertext_size = size_from_js(ciphertext_size, "the sealed size")?;
    let blob = drop_crypto::from_hex(blob).map_err(to_js_error)?;

    drop_crypto::open_metadata(&keys.inner, ciphertext_size, &blob)
        .map(|inner| Metadata { inner })
        .map_err(to_js_error)
}

/// The sending half of one transfer.
#[wasm_bindgen]
pub struct Sealer {
    inner: drop_crypto::Sealer,
}

#[wasm_bindgen]
impl Sealer {
    #[wasm_bindgen(constructor)]
    pub fn new(keys: &SessionKeys, plaintext_size: f64) -> Result<Sealer, JsError> {
        let plaintext_size = size_from_js(plaintext_size, "the payload size")?;

        Ok(Self {
            inner: drop_crypto::Sealer::new(&keys.inner, plaintext_size),
        })
    }

    #[wasm_bindgen(getter, js_name = totalChunks)]
    pub fn total_chunks(&self) -> f64 {
        self.inner.total_chunks() as f64
    }

    /// Seals one chunk. Chunks must be offered in order, and every chunk but
    /// the last must be exactly `chunkPlaintextBytes()` long.
    #[wasm_bindgen(js_name = sealChunk)]
    pub fn seal_chunk(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, JsError> {
        self.inner.seal_chunk(plaintext).map_err(to_js_error)
    }
}

/// The receiving half of one transfer.
#[wasm_bindgen]
pub struct Opener {
    inner: drop_crypto::Opener,
}

#[wasm_bindgen]
impl Opener {
    #[wasm_bindgen(constructor)]
    pub fn new(keys: &SessionKeys, plaintext_size: f64) -> Result<Opener, JsError> {
        let plaintext_size = size_from_js(plaintext_size, "the payload size")?;

        Ok(Self {
            inner: drop_crypto::Opener::new(&keys.inner, plaintext_size),
        })
    }

    /// Opens one chunk, or fails if it was altered, reordered, duplicated, or
    /// sealed under a different key.
    #[wasm_bindgen(js_name = openChunk)]
    pub fn open_chunk(&mut self, sealed: &[u8]) -> Result<Vec<u8>, JsError> {
        self.inner.open_chunk(sealed).map_err(to_js_error)
    }

    /// Confirms every declared chunk arrived.
    ///
    /// Must be called before the result is treated as a complete file: each
    /// chunk authenticating on its own does not mean the stream was not cut
    /// short.
    pub fn finish(&self) -> Result<(), JsError> {
        self.inner.finish().map_err(to_js_error)
    }
}
