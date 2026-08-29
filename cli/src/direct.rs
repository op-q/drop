//! Choosing and setting up a transfer that needs no Drop server.
//!
//! The pieces this assembles all existed before it: the QUIC endpoint in
//! [`crate::transport::quic`], the record layer in
//! [`crate::transport::rendezvous`], and the one-guess policy in
//! [`crate::send`]. What was missing was anybody calling them, which is why
//! none of it was reachable from the binary.
//!
//! # Where a fallback can happen, and where it cannot
//!
//! Narrower than it first looks. **A failed hole punch is not a failed
//! connection** — iroh carries the same QUIC connection over n0's relay when it
//! cannot punch, and neither peer has to do anything about it. So the Drop
//! relay is not a fallback for connectivity. It is a fallback for *rendezvous
//! and setup*: binding, reaching a home relay, publishing, resolving.
//!
//! That puts every decision before the code is printed. Once a sender has shown
//! a code the path is fixed, because a relay-allocated nameplate and a locally
//! drawn one are different strings and the code names one of them. The receiver
//! still decides per transfer, because the nameplate tells it where to look: a
//! record under that nameplate means the sender went direct, and its absence
//! means the sender fell back.

use std::{error::Error, fmt};

use drop_crypto::TransferCode;

use crate::transport::{
    TransportError,
    quic::{QuicEndpoint, QuicTransport},
    rendezvous::{self, MainlineDirectory},
};

/// The store the direct path publishes to, re-exported so a caller assembling
/// a serverless transfer names one module rather than three.
pub use crate::transport::rendezvous::MainlineDirectory as Directory;

/// How many times a colliding nameplate is redrawn before giving up.
///
/// A collision means somebody else's live transfer already published under the
/// same 24 bits. Redrawing is cheap and independent, so three attempts is
/// already implausible to exhaust; more would be waiting out a broken DHT
/// rather than a collision.
const COLLISION_ATTEMPTS: usize = 3;

/// Which carrier a transfer should use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// Direct if it can be set up, the relay if it cannot. The default,
    /// because a person should not have to know what a DHT is.
    Auto,
    /// Direct only. Fails rather than falling back, which is what someone
    /// verifying that no Drop server is involved actually wants.
    Direct,
    /// The relay only. Also what a browser peer needs on the other end.
    Relay,
}

impl Path {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "p2p" | "direct" => Ok(Self::Direct),
            "relay" => Ok(Self::Relay),
            other => Err(format!(
                "unknown transport {other:?}: expected p2p, relay, or auto"
            )),
        }
    }
}

impl fmt::Display for Path {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Direct => "peer-to-peer",
            Self::Relay => "relay",
        })
    }
}

/// A sender that has published where it can be found, and the code that says
/// where that is.
pub struct Published {
    pub endpoint: QuicEndpoint,
    pub code: TransferCode,
}

/// Binds, becomes reachable, and publishes — in that order, and the order is
/// the point.
///
/// Binding is not being reachable: immediately after it an endpoint knows only
/// its local interfaces, and publishing that would advertise a meeting point
/// nobody outside the LAN can use. `online` is what waits for a home relay, and
/// only then is there an address worth putting in a record.
///
/// The caller must not print a code until this returns. A code shown before the
/// record is retrievable sends the receiver to look somewhere empty, and the
/// receiver cannot tell that from a wrong code.
pub async fn publish_sender(directory: &MainlineDirectory) -> Result<Published, TransportError> {
    let endpoint = QuicEndpoint::bind().await?;
    endpoint.online().await?;
    let addr = endpoint.addr();

    let mut last = None;

    for _ in 0..COLLISION_ATTEMPTS {
        let code = TransferCode::generate()
            .map_err(|error| TransportError::Connect(format!("could not draw a code: {error}")))?;

        // Resolving first does two jobs for one round trip: it catches a
        // nameplate somebody else's live transfer is already using, and it
        // proves the DHT answers at all before a code is shown to anybody.
        match rendezvous::resolve(directory, &code).await {
            Ok(Some(_)) => {
                last = Some(TransportError::Connect(
                    "every drawn nameplate was already in use".into(),
                ));
                continue;
            }
            Ok(None) => {}
            Err(error) => return Err(error),
        }

        rendezvous::publish(directory, &code, &addr).await?;

        return Ok(Published { endpoint, code });
    }

    Err(last.unwrap_or_else(|| TransportError::Connect("could not find a free nameplate".into())))
}

/// A dialled sender, and the endpoint the connection to it lives on.
///
/// The endpoint is handed back rather than dropped, and that is load-bearing
/// rather than tidy: a [`QuicTransport`] does not own its endpoint — the sender
/// needs one endpoint to outlive several connections, so closing it belongs to
/// whoever bound it. An endpoint dropped here takes the connection with it, and
/// the peer sees the transfer die the instant it started.
pub struct Dialled {
    pub transport: QuicTransport,
    pub endpoint: QuicEndpoint,
}

/// Looks the sender up and dials it.
///
/// `Ok(None)` means nobody published under this nameplate, which is the signal
/// that the sender fell back to the relay — not that the code is wrong. A wrong
/// code is not detectable here and is not supposed to be: it surfaces at the
/// sealed metadata, which is the whole reason the one-guess checkpoint exists.
pub async fn dial_sender(
    directory: &MainlineDirectory,
    code: &TransferCode,
) -> Result<Option<Dialled>, TransportError> {
    let Some(addr) = rendezvous::resolve(directory, code).await? else {
        return Ok(None);
    };

    let endpoint = QuicEndpoint::bind().await?;
    let transport = endpoint.connect_transfer(addr).await?;

    Ok(Some(Dialled {
        transport,
        endpoint,
    }))
}

/// Says which way a transfer actually travelled.
///
/// Printed rather than inferred, because the two paths differ in latency by
/// enough that a user watching a slow transfer needs to know which one they
/// are on before they can say anything useful about it.
pub fn report(path: &str) {
    eprintln!("Path    {path}");
}

/// Whether falling back to the relay is allowed, and what to say when it is not.
pub fn may_fall_back(path: Path, failure: &dyn Error) -> Result<(), Box<dyn Error + Send + Sync>> {
    match path {
        Path::Auto => Ok(()),
        Path::Direct => Err(format!(
            "no peer-to-peer path could be set up, and --transport p2p forbids \
             falling back to the relay: {failure}"
        )
        .into()),
        // Never reached: a relay transfer does not attempt a direct setup.
        Path::Relay => Err(format!("the relay path failed: {failure}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::Path;

    #[test]
    fn a_transport_choice_accepts_the_names_the_help_advertises() {
        assert_eq!(Path::parse("auto"), Ok(Path::Auto));
        assert_eq!(Path::parse("p2p"), Ok(Path::Direct));
        assert_eq!(Path::parse("direct"), Ok(Path::Direct));
        assert_eq!(Path::parse("relay"), Ok(Path::Relay));
    }

    /// A typo must not silently pick a path, least of all the one that
    /// contacts a server when the user asked for the one that does not.
    #[test]
    fn an_unknown_transport_is_refused_rather_than_defaulted() {
        let error = Path::parse("quic").expect_err("not a name this accepts");
        assert!(error.contains("p2p"), "unhelpful: {error}");
    }

    /// `--transport p2p` is what somebody checking that no Drop server is
    /// involved would use, so it must not quietly satisfy itself with one.
    #[test]
    fn forcing_the_direct_path_refuses_to_fall_back() {
        let failure: Box<dyn std::error::Error> = "the DHT did not answer".into();

        assert!(super::may_fall_back(Path::Auto, failure.as_ref()).is_ok());
        assert!(super::may_fall_back(Path::Direct, failure.as_ref()).is_err());
    }
}
