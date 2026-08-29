//! What a transfer needs from whatever is carrying it.
//!
//! A transfer is a conversation: control frames in both directions, sealed
//! chunks in one, and an end. Everything above this module is written against
//! that conversation rather than against a WebSocket, so the same send and
//! receive paths can run over a different carrier without being rewritten.
//!
//! **Establishing a connection is deliberately not part of the trait.** The
//! relay is reached at an origin URL with a nameplate it allocated over HTTP;
//! a direct connection is reached by resolving a record and punching a hole.
//! Those take arguments with nothing in common, so each transport module owns
//! its own constructor and the choice between them belongs at the one call
//! site that makes it. Forcing them into a shared signature would produce a
//! parameter bag that every implementation ignores half of.
//!
//! The envelope does not appear here. Chunks arrive sealed and leave sealed,
//! and a transport that could tell the difference would be a transport that
//! could read the payload.

use std::{fmt, future::Future};

use serde_json::Value;

pub mod framed;
pub mod quic;
pub mod relay;
pub mod rendezvous;
#[cfg(test)]
pub mod scripted;

/// One thing a peer said.
///
/// Control frames are modelled as JSON values because that is what both sides
/// of the relay protocol already speak; a transport whose wire format is not
/// JSON is expected to map onto these values rather than to leak its framing
/// upwards.
#[derive(Debug)]
pub enum Frame {
    Control(Value),
    Chunk(Vec<u8>),
}

#[derive(Debug)]
pub enum TransportError {
    /// The connection could not be established.
    Connect(String),
    /// The connection failed while a transfer was in progress.
    Io(String),
    /// The peer sent something this transport could not decode. Distinct from
    /// `Io` because it is a peer's mistake rather than the network's.
    Malformed(String),
    /// The far side refused the transfer and said why. The message is theirs
    /// and is passed through verbatim, because it is more specific than
    /// anything this layer could say about it.
    Refused(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(message)
            | Self::Io(message)
            | Self::Malformed(message)
            | Self::Refused(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// A carrier for one transfer.
///
/// The futures are declared `Send` rather than left to inference. The transfer
/// paths are spawned onto a multi-threaded runtime by the CLI's own tests, and
/// an `async fn` in a trait promises nothing about the auto traits of the
/// future it returns, so leaving it out would make the caller's future
/// unspawnable for reasons that point at the call site rather than at here.
pub trait Transport {
    /// Whether the peers themselves have to limit password guessing.
    ///
    /// The security of a 33-bit code rests entirely on an attacker getting one
    /// attempt, and over the relay a third party provides that: `claim_receiver`
    /// refuses a second claim, so a wrong guess burns the session server-side
    /// and this answers `false`. A direct connection has nobody to do that, so
    /// it answers `true` and the peers run the checkpoint after `meta` that
    /// `docs/decisions.md` entry 13 specifies.
    ///
    /// **Deliberately without a default.** Both wrong answers are security bugs
    /// rather than papercuts — `false` on a direct connection is an unlimited
    /// guessing oracle, and `true` over the relay makes the receiver send a
    /// frame the relay rejects, failing every transfer — so a new carrier that
    /// has not thought about it should fail to compile rather than inherit
    /// somebody else's answer.
    fn peers_enforce_one_guess(&self) -> bool;

    /// Waits until the peer is present.
    ///
    /// A carrier that cannot exist without a peer answers immediately — a QUIC
    /// connection is a connection to somebody — so that is the default. A
    /// relay session exists before either side joins, and its transport waits
    /// for the relay to say the other one arrived.
    ///
    /// This is a method rather than a frame the path waits for, and the
    /// distinction is the point. `receiver_connected` is a sentence the relay
    /// invents; no peer ever sends it. A path that blocked on it would be a
    /// path that only works over a relay, which is exactly what this phase is
    /// undoing.
    fn await_peer(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send {
        async { Ok(()) }
    }

    /// Sends a control frame.
    fn send_control(
        &mut self,
        frame: Value,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Sends one sealed chunk.
    fn send_chunk(
        &mut self,
        chunk: Vec<u8>,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Waits for the peer's next frame, or `None` once it has finished
    /// speaking.
    ///
    /// A closed connection is not an error here. Every caller treats a peer
    /// that stopped early as a failure of the transfer rather than of the
    /// transport, and each has a more useful sentence to say about it than
    /// this layer could.
    fn receive(&mut self) -> impl Future<Output = Result<Option<Frame>, TransportError>> + Send;

    /// Ends the conversation politely, ignoring whether the peer was still
    /// listening. Every call site is on a path that has already decided the
    /// transfer's outcome.
    fn close(&mut self) -> impl Future<Output = ()> + Send;
}
