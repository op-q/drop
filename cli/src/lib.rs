//! Terminal client for the Drop ephemeral file-transfer relay.
//!
//! The binary is a thin shell over these modules so the archive format, path
//! safety rules, and transport can be exercised directly by tests.

pub mod cancel;
pub mod client;
pub mod consent;
pub mod direct;
pub mod names;
// Re-exported rather than defined here: the envelope is a separate crate with
// no I/O or transport in reach. Keeping the name `crypto`
// means every call site below reads the same as before the split.
pub use drop_crypto as crypto;
pub mod display;
pub mod payload;
pub mod progress;
pub mod recv;
pub mod send;
pub mod tar;
pub mod transport;
pub mod ui;
pub mod untar;
