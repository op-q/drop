//! Terminal client for the Drop ephemeral file-transfer relay.
//!
//! The binary is a thin shell over these modules so the archive format, path
//! safety rules, and transport can be exercised directly by tests.

pub mod client;
// Re-exported rather than defined here: the envelope is a separate crate so
// the browser client can compile it to WebAssembly. Keeping the name `crypto`
// means every call site below reads the same as before the split.
pub use drop_crypto as crypto;
pub mod payload;
pub mod progress;
pub mod recv;
pub mod send;
pub mod tar;
pub mod untar;
