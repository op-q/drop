//! How two peers find each other with no server to introduce them.
//!
//! The sender publishes its address under a keypair derived from the
//! nameplate; the receiver derives the same keypair and looks it up. The seed
//! comes from [`drop_crypto::rendezvous_secret`], which takes it from the
//! nameplate and never from the words — see that module for why that
//! distinction is the whole design.
//!
//! Three things live here, and only the last one touches a network:
//!
//! - [`publishable`], which decides what may be said about the sender at all.
//! - [`record_for`] and [`address_in`], which build and read the record.
//! - [`Directory`], the seam the mainline DHT sits behind.
//!
//! # Why the DHT is behind a trait
//!
//! Not for future flexibility. The mainline DHT is UDP, and outbound UDP is
//! blocked on the development machine, so a `Directory` that reaches it cannot
//! be exercised there at all. Splitting it out means the record's construction,
//! signing, parsing and address filtering are all tested against
//! [`InMemoryDirectory`] with no network, and the untested surface shrinks to
//! the two calls that genuinely need one.
//!
//! # What may be said about a sender in a record anyone can read
//!
//! A rendezvous record is published under a key derived from the nameplate.
//! The nameplate is 24 bits, so the record is readable by anyone willing to
//! enumerate them — see [`docs/decisions.md`](../../../docs/decisions.md) entry
//! 10. That was accepted for the sender's *public* address, which is the price
//! of finding a peer without a server.
//!
//! It was never accepted for the rest of what `iroh` hands over. An
//! [`EndpointAddr`] carries every local interface the endpoint found: the LAN
//! address, the Docker bridge, the VPN tunnel. Published, that maps the
//! sender's internal network topology for a stranger who guessed a number —
//! which private ranges they use, that they run containers, that they are on a
//! VPN. Entry 14 is the decision to strip it.
//!
//! # This filter fails closed
//!
//! [`TransportAddr`] is `#[non_exhaustive]`, so `iroh` may add address kinds
//! this module has never heard of. Everything not explicitly recognised as safe
//! is therefore withheld, and a new variant is silently *not published* rather
//! than silently published. The cost of failing closed is a transfer that falls
//! back to the relay; the cost of failing open is a disclosure nobody chose.

use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    str::FromStr,
};

use drop_crypto::{TransferCode, rendezvous_secret};
use iroh::{EndpointAddr, TransportAddr};
use iroh_tickets::{Ticket, endpoint::EndpointTicket};
use pkarr::{Keypair, PublicKey, ResolvePolicy, SignedPacket};

use super::TransportError;

/// The DNS name the address is published under.
///
/// Arbitrary, but namespaced so that a key which happens to carry other records
/// is read for Drop's and nothing else.
const TXT_NAME: &str = "_drop";

/// Cache lifetime advertised in the record, in seconds.
///
/// This drives a resolver's client-side cache and has nothing to do with how
/// long the DHT keeps the record — that is set by the storing nodes and is not
/// something either crate defines. Kept short because a sender that rebinds
/// publishes a new address and a receiver holding the old one dials nowhere.
const TTL: u32 = 60;

/// Strips everything from an address that must not be published.
///
/// Keeps the relay URL — it is n0's address, not the sender's — and any IP that
/// is globally routable. Withholds private, loopback, link-local and otherwise
/// non-routable IPs, and any address kind this module does not recognise.
///
/// Fails rather than returning an addressless record. An `EndpointAddr` with an
/// id and no addresses is syntactically fine and operationally useless: a
/// receiver resolves it, learns the sender's identity, and has nowhere to send
/// a packet. That is a silent failure worth making loud, and it is the expected
/// outcome on a machine with no public address and no relay — a LAN-only
/// endpoint, which is exactly what [`super::quic::QuicEndpoint::bind_without_relays`]
/// produces.
pub fn publishable(addr: &EndpointAddr) -> Result<EndpointAddr, TransportError> {
    let kept: Vec<TransportAddr> = addr
        .addrs
        .iter()
        .filter(|candidate| is_publishable(candidate))
        .cloned()
        .collect();

    if kept.is_empty() {
        return Err(TransportError::Connect(
            "this machine has no address worth publishing: no globally routable IP and no relay"
                .into(),
        ));
    }

    Ok(EndpointAddr::from_parts(addr.id, kept))
}

/// Whether one address may go into a public record.
fn is_publishable(addr: &TransportAddr) -> bool {
    match addr {
        // The relay's own address, which discloses nothing about the sender
        // beyond which region they picked.
        TransportAddr::Relay(_) => true,
        TransportAddr::Ip(socket) => is_globally_routable(socket.ip()),
        // Deliberately not a wildcard over the remaining named variants. See
        // the module note: unrecognised means withheld.
        _ => false,
    }
}

/// Whether an IP is one the public internet can route to.
///
/// Written out rather than deferring to `IpAddr::is_global`, which is still
/// unstable, as are the IPv6 predicates that matter most here
/// (`is_unique_local`, `is_unicast_link_local`). Spelling the ranges out means
/// every rejection has a name and a test.
fn is_globally_routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_routable_v4(v4),
        // An IPv4-mapped address is an IPv4 address wearing a hat. Checked as
        // v6 it passes every test below; unmapped it may be 192.168.0.0/16.
        // Missing this is a bypass for the whole filter.
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_routable_v4(v4),
            None => is_routable_v6(v6),
        },
    }
}

fn is_routable_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();

    !(ip.is_private()            // 10/8, 172.16/12, 192.168/16 — the topology leak
        || ip.is_loopback()      // 127/8
        || ip.is_link_local()    // 169.254/16
        || ip.is_unspecified()   // 0.0.0.0
        || ip.is_multicast()     // 224/4
        || ip.is_broadcast()     // 255.255.255.255
        || ip.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || (a == 100 && (64..128).contains(&b))  // 100.64/10, carrier NAT
        || (a == 198 && (18..20).contains(&b))   // 198.18/15, benchmarking
        || a >= 240) // 240/4 reserved, and 0/8 is caught by is_unspecified only
        && a != 0
}

fn is_routable_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];

    !(ip.is_loopback()        // ::1
        || ip.is_unspecified() // ::
        || ip.is_multicast()   // ff00::/8
        || (first & 0xfe00) == 0xfc00   // fc00::/7, unique local — the v6 topology leak
        || (first & 0xffc0) == 0xfe80   // fe80::/10, link local
        || (first == 0x2001 && (ip.segments()[1] & 0xfff0) == 0x0db0)) // 2001:db8::/32, documentation
}

/// The keypair both peers derive, and neither one chose.
///
/// Its private half is derivable by anyone who knows the nameplate, because the
/// nameplate is its only input. That is not a flaw to be fixed here — it is what
/// lets two strangers meet with nothing but the code — but everything built on
/// it has to assume an attacker holds the same key. In particular a resolved
/// record is **not proof of who published it**, and
/// [`super::quic::QuicTransport::peer`] is not an identity check. Authentication
/// is SPAKE2's job and only SPAKE2's job.
fn meeting_keypair(code: &TransferCode) -> Keypair {
    Keypair::from_secret_key(&rendezvous_secret(code))
}

/// Where the sender should be looked for. Derived, not chosen.
pub fn meeting_point(code: &TransferCode) -> PublicKey {
    meeting_keypair(code).public_key()
}

/// Builds the signed record a sender publishes.
///
/// Filters the address first, so nothing that reaches a record has skipped
/// [`publishable`]. Doing it here rather than at the call site is deliberate:
/// there is exactly one way to construct a record, and it is not possible to
/// build one from an unfiltered address by forgetting a step.
pub fn record_for(
    code: &TransferCode,
    addr: &EndpointAddr,
) -> Result<SignedPacket, TransportError> {
    let ticket = EndpointTicket::from(publishable(addr)?).encode_string();

    SignedPacket::builder()
        .txt(
            TXT_NAME.try_into().map_err(|error| {
                TransportError::Malformed(format!("{TXT_NAME} is not a DNS name: {error}"))
            })?,
            ticket.as_str().try_into().map_err(|error| {
                TransportError::Malformed(format!("the ticket is not TXT-safe: {error}"))
            })?,
            TTL,
        )
        .sign(&meeting_keypair(code))
        .map_err(|error| TransportError::Malformed(format!("could not sign the record: {error}")))
}

/// Reads a sender's address back out of a record.
///
/// The signature is checked by `pkarr` before this sees the packet, which
/// proves only that whoever published it held the nameplate-derived key. Since
/// that key is derivable from a public value, it proves nothing about identity.
pub fn address_in(packet: &SignedPacket) -> Result<EndpointAddr, TransportError> {
    let record = packet
        .resource_records(TXT_NAME)
        .next()
        .ok_or_else(|| TransportError::Malformed(format!("no {TXT_NAME} record in the packet")))?;

    let pkarr::dns::rdata::RData::TXT(txt) = &record.rdata else {
        return Err(TransportError::Malformed(
            "the rendezvous record was not a TXT record".into(),
        ));
    };

    let ticket = String::try_from(txt.clone())
        .map_err(|error| TransportError::Malformed(format!("unreadable TXT record: {error}")))?;

    let ticket = EndpointTicket::from_str(&ticket)
        .map_err(|error| TransportError::Malformed(format!("not an endpoint ticket: {error}")))?;

    Ok(ticket.into())
}

/// Where rendezvous records are stored and looked up.
///
/// The seam exists so the record layer above can be tested without a network;
/// see the module note. Deliberately narrow — a `SignedPacket` in and a
/// `SignedPacket` out — so an implementation cannot accidentally take on
/// responsibility for what goes *in* the record, which is where the privacy
/// decisions live.
pub trait Directory {
    /// Stores a record. The sender does this; a receiver that did would clobber
    /// the address it is trying to read.
    fn put(&self, packet: &SignedPacket)
    -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Looks one up. `Ok(None)` means nobody has published there *yet*, which
    /// is the ordinary case for a receiver who typed the code before the sender
    /// finished publishing, and is why this is not an error.
    fn get(
        &self,
        key: &PublicKey,
    ) -> impl Future<Output = Result<Option<SignedPacket>, TransportError>> + Send;
}

/// Publishes the sender's address where the receiver will look.
pub async fn publish<D: Directory>(
    directory: &D,
    code: &TransferCode,
    addr: &EndpointAddr,
) -> Result<(), TransportError> {
    directory.put(&record_for(code, addr)?).await
}

/// Looks the sender up. `Ok(None)` means not published yet, not "wrong code".
pub async fn resolve<D: Directory>(
    directory: &D,
    code: &TransferCode,
) -> Result<Option<EndpointAddr>, TransportError> {
    match directory.get(&meeting_point(code)).await? {
        Some(packet) => address_in(&packet).map(Some),
        None => Ok(None),
    }
}

/// The mainline DHT, which is the store [`docs/decisions.md`](../../../docs/decisions.md)
/// entry 10 specifies.
///
/// `iroh` 1.0 has no DHT of its own — its own pkarr records travel only over
/// n0's HTTP relay — so `pkarr` is used directly rather than through iroh's
/// address lookup. Using iroh's `EndpointInfo::to_pkarr_signed_packet` here
/// would be actively wrong: it discards the endpoint id and reconstructs it
/// from the *signing* key, so under a nameplate-derived key the receiver would
/// recover the nameplate key as the peer's identity and the QUIC handshake
/// would fail TLS verification.
///
/// **Untested.** Every call below needs outbound UDP, which the development
/// machine does not have. The record layer above is covered; this is not.
pub struct MainlineDirectory {
    client: pkarr::Client,
}

impl MainlineDirectory {
    /// One per process, kept alive. The first operation pays the DHT bootstrap
    /// cost — measured at three to five seconds — and later ones are under a
    /// second on the warm client.
    pub fn new() -> Result<Self, TransportError> {
        let mut builder = pkarr::ClientBuilder::default();

        // Defeats pkarr's five-minute cache floor. Without it a receiver can be
        // served a stale address for 300 seconds after the sender republishes,
        // which for a rendezvous measured in seconds is indistinguishable from
        // the sender not being there.
        builder.minimum_ttl(0);

        let client = builder.build().map_err(|error| {
            TransportError::Connect(format!("could not start a DHT client: {error}"))
        })?;

        Ok(Self { client })
    }
}

impl Directory for MainlineDirectory {
    async fn put(&self, packet: &SignedPacket) -> Result<(), TransportError> {
        self.client
            .publish(packet)
            .await
            .map(|_| ())
            .map_err(|error| {
                TransportError::Connect(format!("could not publish the meeting point: {error}"))
            })
    }

    async fn get(&self, key: &PublicKey) -> Result<Option<SignedPacket>, TransportError> {
        match self.client.resolve(key, ResolvePolicy::CacheFirst).await {
            Ok(packet) => Ok(Some(packet)),
            // Nobody has published there. To a receiver this means "the sender
            // has not got that far yet", which is the ordinary case for the
            // seconds between a code being read out and a record landing — so
            // it must not surface as a failure that ends the wait.
            Err(pkarr::errors::ResolveError::NotFound) => Ok(None),
            // Everything else is the network not working, which is a different
            // thing to tell a user and worth keeping distinct.
            Err(error) => Err(TransportError::Connect(format!(
                "could not look up the meeting point: {error}"
            ))),
        }
    }
}

/// A directory that keeps records in memory, for tests.
///
/// Exists because the real one cannot run where the tests do. Everything above
/// the [`Directory`] seam — filtering, ticketing, signing, parsing — is
/// exercised through this, so what stays untested is the DHT and nothing else.
#[cfg(test)]
pub struct InMemoryDirectory {
    records: std::sync::Mutex<std::collections::HashMap<PublicKey, SignedPacket>>,
}

#[cfg(test)]
impl Default for InMemoryDirectory {
    fn default() -> Self {
        Self {
            records: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

#[cfg(test)]
impl InMemoryDirectory {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
impl Directory for InMemoryDirectory {
    async fn put(&self, packet: &SignedPacket) -> Result<(), TransportError> {
        self.records
            .lock()
            .expect("the directory lock")
            .insert(packet.public_key(), packet.clone());
        Ok(())
    }

    async fn get(&self, key: &PublicKey) -> Result<Option<SignedPacket>, TransportError> {
        Ok(self
            .records
            .lock()
            .expect("the directory lock")
            .get(key)
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InMemoryDirectory, address_in, is_globally_routable, meeting_point, publish, publishable,
        record_for, resolve,
    };
    use drop_crypto::TransferCode;
    use iroh::{EndpointAddr, EndpointId, TransportAddr};
    use std::net::SocketAddr;

    fn code(text: &str) -> TransferCode {
        TransferCode::parse(text).expect("a well-formed code")
    }

    /// A stable identity to hang addresses off. Its value is irrelevant; only
    /// that the filter preserves it.
    fn an_endpoint() -> EndpointId {
        iroh::SecretKey::generate().public()
    }

    fn ip(text: &str) -> TransportAddr {
        TransportAddr::Ip(text.parse::<SocketAddr>().expect("a socket address"))
    }

    fn relay() -> TransportAddr {
        TransportAddr::Relay(
            "https://euw1.relay.iroh.link."
                .parse::<iroh::RelayUrl>()
                .expect("a relay url"),
        )
    }

    /// The load-bearing test in this module, and the one entry 14 exists for.
    ///
    /// Every one of these is a range whose presence in a public record tells a
    /// stranger something about the sender's network that they had no way to
    /// ask for. If this test ever fails, a `drop send` is broadcasting the
    /// shape of somebody's LAN to anyone counting to 2^24.
    #[test]
    fn no_private_range_ever_reaches_a_record() {
        let leaks = [
            "192.168.1.23:41641",          // the home LAN
            "10.8.0.6:41641",              // a VPN tunnel
            "172.17.0.1:41641",            // docker0
            "127.0.0.1:41641",             // loopback
            "169.254.10.1:41641",          // link local
            "100.64.0.1:41641",            // carrier-grade NAT
            "[fd12:3456::1]:41641",        // v6 unique local
            "[fe80::1]:41641",             // v6 link local
            "[::1]:41641",                 // v6 loopback
            "[::ffff:192.168.1.23]:41641", // v4 private wearing a v6 hat
        ];

        for leak in leaks {
            let addr = EndpointAddr::from_parts(an_endpoint(), [ip(leak), relay()]);
            let published = publishable(&addr).expect("the relay survives");

            assert!(
                !published.addrs.contains(&ip(leak)),
                "{leak} must never be published"
            );
        }
    }

    #[test]
    fn a_public_address_and_a_relay_both_survive() {
        let addr = EndpointAddr::from_parts(
            an_endpoint(),
            [ip("192.168.1.23:41641"), relay(), ip("8.8.8.8:41641")],
        );

        let published = publishable(&addr).expect("something survives");

        assert!(published.addrs.contains(&ip("8.8.8.8:41641")));
        assert!(published.addrs.contains(&relay()));
        assert_eq!(published.addrs.len(), 2, "and nothing else");
        assert_eq!(
            published.id, addr.id,
            "the identity is not what is filtered"
        );
    }

    /// The ranges RFC 5737 and RFC 3849 reserve for documentation are not
    /// anybody's address. Withholding them leaks nothing, and it keeps a
    /// copy-pasted example from becoming a record nobody can reach.
    #[test]
    fn documentation_ranges_are_not_addresses() {
        for text in ["192.0.2.1", "198.51.100.7", "203.0.113.44", "2001:db8::1"] {
            let parsed = text.parse().expect("an ip");
            assert!(
                !is_globally_routable(parsed),
                "{text} is a documentation range"
            );
        }
    }

    /// Ordinary public addresses have to survive, or the filter is just an
    /// elaborate way of never publishing anything.
    #[test]
    fn ordinary_public_addresses_survive() {
        for text in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
        ] {
            let parsed = text.parse().expect("an ip");
            assert!(is_globally_routable(parsed), "{text} is publishable");
        }
    }

    /// A LAN-only endpoint has nothing publishable, and that has to be an error
    /// rather than an empty record a receiver would resolve and find useless.
    #[test]
    fn an_endpoint_with_nothing_routable_refuses_to_publish() {
        let addr = EndpointAddr::from_parts(
            an_endpoint(),
            [ip("192.168.1.23:41641"), ip("127.0.0.1:41641")],
        );

        let Err(error) = publishable(&addr) else {
            panic!("an addressless record must not be produced");
        };
        assert!(
            error.to_string().contains("no address worth publishing"),
            "the error should say why: {error}"
        );
    }

    /// A relay alone is enough. This is the ordinary case for a machine behind
    /// NAT, which is most of them.
    #[test]
    fn a_relay_alone_is_publishable() {
        let addr = EndpointAddr::from_parts(an_endpoint(), [relay(), ip("10.0.0.4:41641")]);

        let published = publishable(&addr).expect("the relay is enough");
        assert_eq!(published.addrs.len(), 1);
        assert!(published.addrs.contains(&relay()));
    }

    /// The whole rendezvous, with a directory instead of a DHT.
    ///
    /// Sender publishes, receiver resolves, and the address that comes back is
    /// the one that went in. Everything but the network is real here: the same
    /// derivation, the same ticket encoding, the same signature.
    #[tokio::test]
    async fn a_sender_publishes_where_the_receiver_looks() {
        let directory = InMemoryDirectory::new();
        let code = code("4607F9-abandon-ability-able");
        let addr = EndpointAddr::from_parts(an_endpoint(), [relay(), ip("8.8.8.8:41641")]);

        publish(&directory, &code, &addr).await.expect("published");

        let found = resolve(&directory, &code)
            .await
            .expect("resolved")
            .expect("a record was there");

        assert_eq!(
            found.id, addr.id,
            "the sender's identity survives the round trip"
        );
        assert!(found.addrs.contains(&relay()));
        assert!(found.addrs.contains(&ip("8.8.8.8:41641")));
    }

    /// A receiver that arrives before the sender has published must be told
    /// "not yet", not "wrong code". They are the same to a resolver and
    /// completely different to a human, and the wait loop depends on it.
    #[tokio::test]
    async fn an_unpublished_code_resolves_to_nothing_rather_than_failing() {
        let directory = InMemoryDirectory::new();

        let found = resolve(&directory, &code("4607F9-abandon-ability-able"))
            .await
            .expect("a miss is not an error");

        assert!(found.is_none(), "nobody has published there");
    }

    /// The property the whole split code rests on, now enforced end to end
    /// rather than only in the derivation: two people holding the same
    /// nameplate meet, whatever words they think they have.
    ///
    /// This is not a nice-to-have. It is why a receiver who mistypes a word
    /// still reaches the sender and fails at the key exchange — where the
    /// failure is safe and countable — instead of silently looking somewhere
    /// nobody published, which would be indistinguishable from the sender being
    /// offline.
    #[tokio::test]
    async fn the_words_do_not_change_where_two_peers_meet() {
        let sender = code("4607F9-abandon-ability-able");
        let mistyped = code("4607F9-zone-zoo-zebra");

        assert_eq!(meeting_point(&sender), meeting_point(&mistyped));

        let directory = InMemoryDirectory::new();
        let addr = EndpointAddr::from_parts(an_endpoint(), [relay()]);

        publish(&directory, &sender, &addr)
            .await
            .expect("published");

        let found = resolve(&directory, &mistyped)
            .await
            .expect("resolved")
            .expect("a mistyped word still finds the sender");

        assert_eq!(found.id, addr.id);
    }

    /// A different nameplate is a different place, or the 24 bits would not be
    /// doing anything.
    #[tokio::test]
    async fn a_different_nameplate_finds_nothing() {
        let directory = InMemoryDirectory::new();
        let addr = EndpointAddr::from_parts(an_endpoint(), [relay()]);

        publish(&directory, &code("4607F9-abandon-ability-able"), &addr)
            .await
            .expect("published");

        let found = resolve(&directory, &code("7F2A91-abandon-ability-able"))
            .await
            .expect("a miss is not an error");

        assert!(
            found.is_none(),
            "a neighbouring nameplate is not a neighbour"
        );
    }

    /// The filter is not something a caller can forget: there is one way to
    /// build a record, and it filters. A private address handed to `record_for`
    /// must not come back out of `address_in`.
    #[test]
    fn a_record_cannot_be_built_around_the_filter() {
        let code = code("4607F9-abandon-ability-able");
        let addr = EndpointAddr::from_parts(
            an_endpoint(),
            [relay(), ip("192.168.1.23:41641"), ip("172.17.0.1:41641")],
        );

        let packet = record_for(&code, &addr).expect("a record");
        let read_back = address_in(&packet).expect("readable");

        assert_eq!(
            read_back.addrs.len(),
            1,
            "only the relay should have survived"
        );
        assert!(read_back.addrs.contains(&relay()));
    }

    /// A LAN-only sender cannot publish at all, and finding that out at publish
    /// time is the point — the alternative is a receiver resolving a record
    /// with nowhere to dial.
    #[tokio::test]
    async fn a_lan_only_sender_cannot_publish() {
        let directory = InMemoryDirectory::new();
        let addr = EndpointAddr::from_parts(an_endpoint(), [ip("192.168.1.23:41641")]);

        let Err(error) = publish(&directory, &code("4607F9-abandon-ability-able"), &addr).await
        else {
            panic!("a record with nothing routable in it must not be published");
        };
        assert!(error.to_string().contains("no address worth publishing"));
    }

    /// The record is signed by the derived key, so anyone who guesses the
    /// nameplate can produce a valid one. Pinned here so that nobody later
    /// mistakes a verified signature for a verified sender.
    #[test]
    fn the_record_is_signed_by_a_key_anyone_can_derive() {
        let code = code("4607F9-abandon-ability-able");
        let packet = record_for(&code, &EndpointAddr::from_parts(an_endpoint(), [relay()]))
            .expect("a record");

        assert_eq!(
            packet.public_key(),
            meeting_point(&code),
            "signed by the meeting key, which is derived from a public nameplate \
             and therefore proves nothing about who published it"
        );
    }
}
