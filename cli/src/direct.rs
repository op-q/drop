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

use std::{
    env,
    error::Error,
    fmt,
    net::{SocketAddr, SocketAddrV4, ToSocketAddrs},
};

use drop_crypto::TransferCode;
use iroh::{RelayMode, RelayUrl};

use crate::transport::{
    TransportError,
    quic::{QuicEndpoint, QuicTransport},
    rendezvous::{self, MainlineDirectory},
};

/// The store the direct path publishes to, re-exported so a caller assembling
/// a serverless transfer names one module rather than three.
pub use crate::transport::rendezvous::MainlineDirectory as Directory;

/// The iroh relay to use, instead of n0's.
pub const RELAY_VAR: &str = "DROP_RENDEZVOUS_RELAY";

/// DHT nodes to bootstrap from, comma separated, instead of the public routers.
pub const BOOTSTRAP_VAR: &str = "DROP_RENDEZVOUS_BOOTSTRAP";

/// Where the direct path looks for the infrastructure that introduces two
/// peers.
///
/// Two independent values, because rendezvous is two jobs done by two
/// unrelated systems. The **relay** is how an endpoint becomes reachable at
/// all and how two peers behind NAT are put in touch; the **DHT** is where the
/// sender leaves an address for the receiver to find. Moving one does not
/// imply moving the other, and a deployment may well want to.
///
/// Both default to the public infrastructure that shipped before this existed,
/// so a `drop` with neither variable set is byte-for-byte the same program it
/// was. See
/// [`docs/plans/self-hosted-rendezvous-plan-2026-09-10.md`](../../docs/plans/self-hosted-rendezvous-plan-2026-09-10.md)
/// for why this is configuration a deployment needs rather than a knob for a
/// test, and for what an operator takes on by pointing it somewhere.
///
/// Note what this does **not** do: nothing here relaxes
/// [`rendezvous::publishable`]. A self-hosted relay works with that filter
/// rather than around it, because `TransportAddr::Relay(_)` is publishable
/// unconditionally — so an endpoint whose every IP is private still produces a
/// usable record naming the relay, and no private address reaches the DHT.
#[derive(Clone, Debug, Default)]
pub struct Rendezvous {
    /// `None` is n0's relays.
    relay: Option<RelayUrl>,
    /// Resolved at parse time, not at use. Empty is pkarr's public routers.
    bootstrap: Vec<SocketAddrV4>,
}

impl Rendezvous {
    /// Reads both variables, failing rather than falling back.
    ///
    /// **A malformed value is an error and never a silent default**, and that
    /// is the single most important decision in this type. An operator sets
    /// these to keep rendezvous inside their own network; a typo that quietly
    /// reverted to n0 and the public DHT would publish to exactly the place
    /// they were trying to avoid, and would do it invisibly. An error costs
    /// them a restart, and the alternative costs them the thing they
    /// configured.
    ///
    /// An unset variable, and an empty one, both mean the default — a shell
    /// that exports an empty string has said nothing, not "use nothing".
    pub fn from_env() -> Result<Self, String> {
        Self::parse(
            env::var(RELAY_VAR).ok().as_deref(),
            env::var(BOOTSTRAP_VAR).ok().as_deref(),
        )
    }

    /// Split out from [`Self::from_env`] so it can be tested without touching
    /// the process environment, which is global and would make these tests
    /// order-dependent against every other test in the binary.
    fn parse(relay: Option<&str>, bootstrap: Option<&str>) -> Result<Self, String> {
        let relay = match relay.map(str::trim).filter(|value| !value.is_empty()) {
            None => None,
            Some(value) => Some(parse_relay_url(value)?),
        };

        let mut nodes = Vec::new();
        for entry in bootstrap.unwrap_or_default().split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }

            // Resolved here rather than handed to pkarr as text, because
            // pkarr resolves too and *discards* what it cannot resolve. A
            // mistyped hostname would leave it with an empty list, which it
            // reads as an instruction to form its own DHT — see
            // [`MainlineDirectory::new`]. Failing here turns that silent
            // isolation into a message naming the entry.
            let resolved: Vec<SocketAddr> = entry
                .to_socket_addrs()
                .map_err(|error| format!("{BOOTSTRAP_VAR} entry {entry:?} did not resolve: {error}. Expected host:port"))?
                .collect();

            let before = nodes.len();
            nodes.extend(resolved.iter().filter_map(|addr| match addr {
                SocketAddr::V4(v4) => Some(*v4),
                // pkarr's DHT is IPv4-only, and it drops v6 without comment.
                SocketAddr::V6(_) => None,
            }));

            if nodes.len() == before {
                return Err(format!(
                    "{BOOTSTRAP_VAR} entry {entry:?} resolved only to IPv6, and the DHT is IPv4"
                ));
            }
        }

        Ok(Self {
            relay,
            bootstrap: nodes,
        })
    }

    /// How an endpoint should be bound to reach this rendezvous.
    pub fn relay_mode(&self) -> RelayMode {
        match &self.relay {
            Some(url) => RelayMode::custom([url.clone()]),
            None => RelayMode::Default,
        }
    }

    /// The record store this rendezvous names.
    pub fn directory(&self) -> Result<MainlineDirectory, TransportError> {
        MainlineDirectory::new(&self.bootstrap)
    }
}

/// Parses a relay URL, and checks the things `RelayUrl` does not.
///
/// **`RelayUrl` validates nothing beyond URL syntax.** Its `FromStr` is
/// `Url::from_str` and then a wrapper, so `relay.example:3340` parses
/// *successfully* into a URL whose scheme is `relay.example` and whose path is
/// `3340`. Binding an endpoint to that does not fail either — iroh simply never
/// reaches a relay, `online` times out after fifteen seconds, and the transfer
/// falls back to the Drop relay. An operator would see a slow transfer and no
/// explanation.
///
/// A forgotten scheme is the likeliest thing anyone will type, so it is the one
/// case that must not be quietly accepted.
fn parse_relay_url(value: &str) -> Result<RelayUrl, String> {
    let url: RelayUrl = value.parse().map_err(|error| {
        format!("{RELAY_VAR} is not a URL: {error}. Expected http://host:port or https://host")
    })?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "{RELAY_VAR} has scheme {:?}, and an iroh relay is reached over http or https. \
             A missing scheme parses as one: {value:?} names no host.",
            url.scheme()
        ));
    }

    if url.host().is_none() {
        return Err(format!("{RELAY_VAR} names no host: {value:?}"));
    }

    Ok(url)
}

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
pub async fn publish_sender(
    directory: &MainlineDirectory,
    rendezvous: &Rendezvous,
) -> Result<Published, TransportError> {
    let endpoint = QuicEndpoint::bind(rendezvous.relay_mode()).await?;
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
    rendezvous: &Rendezvous,
) -> Result<Option<Dialled>, TransportError> {
    let Some(addr) = rendezvous::resolve(directory, code).await? else {
        return Ok(None);
    };

    let endpoint = QuicEndpoint::bind(rendezvous.relay_mode()).await?;
    let transport = endpoint.connect_transfer(addr).await?;

    Ok(Some(Dialled {
        transport,
        endpoint,
    }))
}

/// Which carrier actually moved the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    /// Two terminals, no Drop process anywhere in the path.
    Direct,
    /// Through a Drop relay, which carries an envelope it cannot read.
    Relay,
}

impl Carrier {
    /// What a person reads.
    fn prose(self) -> &'static str {
        match self {
            Self::Direct => "peer-to-peer (no Drop server)",
            Self::Relay => "relay (encrypted; the relay cannot read it)",
        }
    }

    /// What a program reads. Short, lowercase, and the same spelling
    /// `--transport` already accepts, so one vocabulary covers asking for a
    /// path and being told which one happened.
    fn tag(self) -> &'static str {
        match self {
            Self::Direct => "p2p",
            Self::Relay => "relay",
        }
    }
}

/// Why the direct path was not the one taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// Nothing was fallen back from. Either the direct path worked, or the
    /// relay is what was asked for.
    None,
    /// A direct path could not be set up: binding, becoming reachable,
    /// publishing, or a directory that did not answer.
    Rendezvous,
    /// Receiver-side only. Nobody had published under this nameplate, which
    /// means the sender itself fell back before it printed a code. Distinct
    /// from [`Self::Rendezvous`] because nothing here failed — this receiver
    /// looked in the right place and correctly found the sender absent.
    NoRecord,
}

impl Fallback {
    fn tag(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Rendezvous => "rendezvous",
            Self::NoRecord => "no-record",
        }
    }
}

/// Says which way a transfer actually travelled.
///
/// Printed rather than inferred, because the two paths differ in latency by
/// enough that a user watching a slow transfer needs to know which one they
/// are on before they can say anything useful about it.
///
/// `status` adds [`status_line`] beside the prose rather than instead of it.
/// A person gets the sentence; a program gets the line.
pub fn report(carrier: Carrier, fallback: Fallback, status: bool) {
    eprintln!("Path    {}", carrier.prose());

    if status {
        eprintln!("{}", status_line(carrier, fallback));
    }
}

/// The machine-readable half of [`report`].
///
/// Deliberately one line of `key=value` rather than a `--json` mode. A JSON
/// object invites a reporting schema to grow inside it, and then the schema is
/// a compatibility surface; what anything actually needs to know is which
/// carrier moved the bytes and why it was not the other one. One line is also
/// greppable from a shell, which is where this gets read.
///
/// Rendered rather than printed so it can be tested without capturing a
/// stream.
pub fn status_line(carrier: Carrier, fallback: Fallback) -> String {
    format!(
        "drop-status: path={} fallback={}",
        carrier.tag(),
        fallback.tag()
    )
}

/// Whether falling back to the relay is allowed, and what to say when it is not.
pub fn may_fall_back(
    path: Path,
    relay: Option<&str>,
    failure: &dyn Error,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match (path, relay) {
        (Path::Auto, Some(_)) => Ok(()),
        // The ordinary case now, and it is not the same failure as `p2p`
        // refusing: nobody asked for the direct path only, so a person who
        // hits this needs to be told a relay is missing rather than forbidden.
        (Path::Auto, None) => Err(format!(
            "no peer-to-peer path could be set up, and no relay is configured to fall back \
             to: {failure}. To use one, {}.",
            crate::client::NAME_A_RELAY
        )
        .into()),
        (Path::Direct, _) => Err(format!(
            "no peer-to-peer path could be set up, and --transport p2p forbids \
             falling back to the relay: {failure}"
        )
        .into()),
        // Never reached: a relay transfer does not attempt a direct setup.
        (Path::Relay, _) => Err(format!("the relay path failed: {failure}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{BOOTSTRAP_VAR, Carrier, Fallback, Path, RELAY_VAR, Rendezvous, status_line};
    use iroh::RelayMode;

    /// Nothing set is the shipped behaviour, and that has to be asserted rather
    /// than assumed: this type's whole claim is that a deployment which does
    /// not ask for anything sees no change.
    #[test]
    fn an_unconfigured_rendezvous_is_the_public_one() {
        let default = Rendezvous::parse(None, None).expect("nothing to parse");

        assert!(default.relay.is_none(), "no relay means n0's");
        assert!(
            default.bootstrap.is_empty(),
            "no nodes means the public DHT"
        );
        assert!(matches!(default.relay_mode(), RelayMode::Default));
    }

    /// An exported empty string is a shell having said nothing, which is not
    /// the same as having said "use nothing".
    ///
    /// This matters more for the bootstrap list than it looks. pkarr reads an
    /// empty node list as an instruction to form its *own* DHT, where a sender
    /// publishes happily into a network of one and no receiver can ever resolve
    /// it — so an empty variable reaching pkarr unchanged would turn
    /// `export DROP_RENDEZVOUS_BOOTSTRAP=` into a transfer that silently never
    /// connects.
    #[test]
    fn an_empty_variable_means_the_default_and_not_an_empty_network() {
        let empty = Rendezvous::parse(Some(""), Some("")).expect("empty is not malformed");

        assert!(empty.relay.is_none());
        assert!(empty.bootstrap.is_empty());

        let spaces = Rendezvous::parse(Some("   "), Some("  ,  ")).expect("whitespace is nothing");

        assert!(spaces.relay.is_none());
        assert!(spaces.bootstrap.is_empty(), "a lone comma names no node");
    }

    #[test]
    fn a_configured_relay_replaces_the_default_map() {
        let configured =
            Rendezvous::parse(Some("http://10.0.0.9:3340"), None).expect("a relay URL");

        let RelayMode::Custom(map) = configured.relay_mode() else {
            panic!("a configured relay must not leave the default map in place");
        };
        assert_eq!(map.len(), 1, "one relay was named, so one is used");
    }

    /// Bootstrap nodes are resolved at parse time, so a literal address comes
    /// through as itself and the count is the count.
    #[test]
    fn bootstrap_nodes_are_taken_in_order_and_a_trailing_comma_is_tolerated() {
        let configured = Rendezvous::parse(None, Some("10.0.0.9:6881, 10.0.0.10:6881,"))
            .expect("two literal addresses");

        assert_eq!(
            configured
                .bootstrap
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["10.0.0.9:6881", "10.0.0.10:6881"]
        );
    }

    /// The sharp edge of this whole feature, in two tests.
    ///
    /// An operator sets these to keep rendezvous inside their network. A typo
    /// that fell back to the default would publish to n0 and the public DHT —
    /// the exact place they were configuring their way out of — and would do it
    /// with no output. So a malformed value fails, and the message names the
    /// variable so they know which of the two to look at.
    ///
    /// The forgotten scheme is the case this exists for, and it is the one a
    /// naive check misses: `RelayUrl::from_str` is `Url::from_str`, so
    /// `relay.example:3340` *parses*, into a URL whose scheme is
    /// `relay.example`. Nothing downstream rejects it either — the endpoint
    /// binds, no relay is ever reached, `online` spends fifteen seconds timing
    /// out, and the transfer falls back. Accepting it would have produced
    /// exactly the invisible downgrade this type is written to prevent.
    #[test]
    fn a_relay_url_with_no_scheme_is_refused_rather_than_parsed_into_nonsense() {
        let error = Rendezvous::parse(Some("relay.example:3340"), None)
            .expect_err("a bare host is not a relay URL, however well it parses");

        assert!(error.contains(RELAY_VAR), "unhelpful: {error}");
        assert!(
            error.contains("scheme"),
            "it should say what is wrong: {error}"
        );
    }

    /// And an unrelated scheme is refused for the same reason: iroh reaches a
    /// relay over HTTP, and anything else would bind cleanly and never connect.
    #[test]
    fn a_relay_url_with_the_wrong_scheme_is_refused() {
        for value in ["ws://10.0.0.9:3340", "ftp://10.0.0.9"] {
            let error = Rendezvous::parse(Some(value), None)
                .expect_err("an iroh relay is not reached over this scheme");
            assert!(error.contains(RELAY_VAR), "unhelpful for {value}: {error}");
        }
    }

    /// Both spellings a relay is actually reached by have to work, or the
    /// validation above is just a way of refusing every input.
    #[test]
    fn http_and_https_relays_are_both_accepted() {
        for value in ["http://10.0.0.9:3340", "https://relay.example"] {
            let parsed = Rendezvous::parse(Some(value), None)
                .unwrap_or_else(|error| panic!("{value} should be accepted: {error}"));
            assert!(parsed.relay.is_some(), "{value} produced no relay");
        }
    }

    #[test]
    fn a_malformed_bootstrap_entry_fails_and_names_the_entry() {
        let error = Rendezvous::parse(None, Some("10.0.0.9:6881,10.0.0.10"))
            .expect_err("an address with no port is not a node");

        assert!(error.contains(BOOTSTRAP_VAR), "unhelpful: {error}");
        assert!(
            error.contains("10.0.0.10"),
            "it should say which entry: {error}"
        );
    }

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

        let relay = Some("https://relay.example");

        assert!(super::may_fall_back(Path::Auto, relay, failure.as_ref()).is_ok());
        assert!(super::may_fall_back(Path::Direct, relay, failure.as_ref()).is_err());

        // Auto with nothing to fall back to is an error, and says which of the
        // two reasons it is: missing, not forbidden.
        let missing = super::may_fall_back(Path::Auto, None, failure.as_ref())
            .expect_err("auto cannot fall back to a relay nobody named");
        let missing = missing.to_string();

        assert!(
            missing.contains("no relay is configured"),
            "unhelpful: {missing}"
        );
        assert!(!missing.contains("forbids"), "wrong reason: {missing}");
    }

    /// Every combination that can actually be printed, spelled out.
    ///
    /// This is the whole point of the line: something reading it is reading an
    /// exact string, so a reworded prose message must not be able to change it
    /// and neither must a refactor here. Written as literals rather than built
    /// from the same `tag` methods the code uses, because a test that composes
    /// the answer the same way the implementation does agrees with it by
    /// construction and checks nothing.
    #[test]
    fn the_status_line_says_which_carrier_and_why() {
        assert_eq!(
            status_line(Carrier::Direct, Fallback::None),
            "drop-status: path=p2p fallback=none"
        );
        assert_eq!(
            status_line(Carrier::Relay, Fallback::None),
            "drop-status: path=relay fallback=none"
        );
        assert_eq!(
            status_line(Carrier::Relay, Fallback::Rendezvous),
            "drop-status: path=relay fallback=rendezvous"
        );
        assert_eq!(
            status_line(Carrier::Relay, Fallback::NoRecord),
            "drop-status: path=relay fallback=no-record"
        );
    }
}
