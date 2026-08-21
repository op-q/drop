//! Terminal client for the Drop ephemeral file-transfer relay.
//!
//! The binary is a thin shell over these modules so the archive format, path
//! safety rules, and transport can be exercised directly by tests.

pub mod client;
pub mod crypto;
pub mod payload;
pub mod progress;
pub mod recv;
pub mod send;
pub mod tar;
pub mod untar;
