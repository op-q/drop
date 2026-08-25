//! What may be said about a sender in a record anyone can read.
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

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use iroh::{EndpointAddr, TransportAddr};

use super::TransportError;

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

#[cfg(test)]
mod tests {
    use super::{is_globally_routable, publishable};
    use iroh::{EndpointAddr, EndpointId, TransportAddr};
    use std::net::SocketAddr;

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
}
