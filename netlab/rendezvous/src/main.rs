//! An iroh relay and a mainline DHT, bound where the lab can reach them.
//!
//! # Why this exists
//!
//! Drop's direct path needs two pieces of third-party infrastructure before two
//! peers can meet: a relay to become reachable through, and a DHT to leave an
//! address in. In the field both are public — n0's relays and the mainline
//! bootstrap routers — and an isolated network namespace has neither. That is
//! finding 2 of `docs/plans/network-lab-plan-2026-08-31.md`, and it is why half
//! that plan's topology matrix could not be built.
//!
//! So the lab runs its own. Pointed at by `DROP_RENDEZVOUS_RELAY` and
//! `DROP_RENDEZVOUS_BOOTSTRAP`, which exist because a deployment inside an
//! egress-filtered network needs them too — see
//! `docs/plans/self-hosted-rendezvous-plan-2026-09-10.md`.
//!
//! # What it is not
//!
//! **Not a reimplementation of anything.** Both halves are the real crates
//! Drop's CLI talks to in production, configured to bind somewhere else. This
//! file is argument parsing and two constructor calls, which is the most it is
//! allowed to be: the lab's rule is that no part of the Drop protocol is
//! written a second time, and that rule covers the infrastructure Drop talks to
//! just as much as the protocol itself.
//!
//! **Not the mainline DHT.** Three nodes that know only each other. It answers
//! the same queries with the same wire protocol and it has none of the real
//! network's size, churn, latency or hostility. Rendezvous timing measured
//! against this says nothing about rendezvous timing in the field.
//!
//! # Plain HTTP for the relay, TLS for address discovery
//!
//! The relay itself is served over `http://` with no TLS, as `iroh-relay`'s own
//! `--dev` mode does, because a CA-signed certificate for a namespace-local
//! address is not a thing that exists.
//!
//! **QUIC address discovery still needs TLS**, and it cannot be skipped: behind
//! NAT at both ends, a peer's only candidate addresses are private ones the
//! lab's core router has no route to, so without discovery neither peer ever
//! learns a translated address and no punch can even be attempted. The two NAT
//! topologies would then be indistinguishable — both relayed — and the lab
//! would be measuring its own omission.
//!
//! So the QUIC endpoint gets its own self-signed certificate, which works
//! because iroh's QUIC client does not verify relays against a public root:
//! `iroh/src/tls.rs` installs a custom verifier, since iroh authenticates peers
//! by endpoint id rather than by certificate chain. The certificate names the
//! bind address rather than `localhost`, which is why `rcgen` is a direct
//! dependency instead of `iroh-relay`'s own test helper.
//!
//! The port is **7842** and is not a choice. `RelayMode::custom` — what
//! `DROP_RENDEZVOUS_RELAY` turns into — builds its relay map from URLs alone,
//! and every entry gets `RelayQuicConfig::default()`, whose port is
//! `DEFAULT_RELAY_QUIC_PORT`. The client will look there and nowhere else.

use std::{
    net::{Ipv4Addr, SocketAddr},
    process::ExitCode,
};

use iroh_relay::server::{QuicConfig, RelayConfig, Server, ServerConfig};
use mainline::Testnet;

/// Where the QUIC address-discovery endpoint must listen.
///
/// Not a choice. `RelayMode::custom` builds its map from URLs alone and gives
/// every entry `RelayQuicConfig::default()`, whose port is
/// `iroh_relay::defaults::DEFAULT_RELAY_QUIC_PORT`. A client configured by
/// `DROP_RENDEZVOUS_RELAY` will probe this port and no other, so serving
/// anywhere else is the same as not serving at all.
const QAD_PORT: u16 = 7842;

/// A certificate naming the address the lab actually binds.
///
/// `iroh-relay`'s own test helper issues one for `localhost` and `127.0.0.1`,
/// which is the wrong name for a namespace address. It very likely would not
/// matter — iroh installs a custom verifier for relay connections, because it
/// authenticates peers by endpoint id rather than by certificate chain — but
/// "very likely" is not worth spending on the one thing that decides whether
/// either NAT topology can punch at all.
fn self_signed_for(address: Ipv4Addr) -> Result<rustls::ServerConfig, String> {
    let certificate = rcgen::generate_simple_self_signed(vec![address.to_string()])
        .map_err(|error| format!("could not issue a certificate for {address}: {error}"))?;

    let key = rustls::pki_types::PrivateKeyDer::from(rustls::pki_types::PrivatePkcs8KeyDer::from(
        certificate.signing_key.serialize_der(),
    ));

    rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| format!("ring does not support the required TLS versions: {error}"))?
    .with_no_client_auth()
    .with_single_cert(vec![certificate.cert.der().clone()], key)
    .map_err(|error| format!("the generated certificate was rejected: {error}"))
}

const USAGE: &str = "\
netlab-rendezvous — an iroh relay and a mainline DHT for the network lab

USAGE
    netlab-rendezvous [--bind <IPV4>] [--relay-port <PORT>] [--dht-nodes <N>]

OPTIONS
    --bind <IPV4>       Address to bind both services to [default: 127.0.0.1]
    --relay-port <PORT> Relay HTTP port, 0 for whichever is free [default: 0]
    --dht-nodes <N>     DHT nodes to run [default: 3]

OUTPUT
    Three lines on stdout, flushed, then it runs until killed:

        netlab-rendezvous: relay=http://127.0.0.1:39481
        netlab-rendezvous: bootstrap=127.0.0.1:45001,127.0.0.1:45002
        netlab-rendezvous: ready

    The addresses are reported rather than assumed because every port here can
    be ephemeral. A harness reads these two lines into DROP_RENDEZVOUS_RELAY
    and DROP_RENDEZVOUS_BOOTSTRAP, and waits for `ready` before starting a
    transfer — a `drop` that publishes before the DHT is listening gets a
    rendezvous failure that looks exactly like a broken network.
";

/// How many DHT nodes to run when nobody says.
///
/// Three, because pkarr's own tests use three and a single node is not a
/// network: several of its queries want more than one peer to ask before they
/// will call an answer settled.
const DEFAULT_DHT_NODES: usize = 3;

struct Options {
    bind: Ipv4Addr,
    relay_port: u16,
    dht_nodes: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bind: Ipv4Addr::LOCALHOST,
            relay_port: 0,
            dht_nodes: DEFAULT_DHT_NODES,
        }
    }
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut index = 0;

    while index < arguments.len() {
        let flag = arguments[index].as_str();
        let value = || {
            arguments
                .get(index + 1)
                .ok_or_else(|| format!("{flag} needs a value"))
        };

        match flag {
            "--bind" => {
                options.bind = value()?
                    .parse()
                    .map_err(|error| format!("--bind is not an IPv4 address: {error}"))?;
            }
            "--relay-port" => {
                options.relay_port = value()?
                    .parse()
                    .map_err(|error| format!("--relay-port is not a port: {error}"))?;
            }
            "--dht-nodes" => {
                options.dht_nodes = value()?
                    .parse()
                    .map_err(|error| format!("--dht-nodes is not a count: {error}"))?;
                if options.dht_nodes == 0 {
                    return Err("--dht-nodes 0 would leave no DHT to publish into".into());
                }
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other:?}")),
        }

        index += 2;
    }

    Ok(options)
}

#[tokio::main]
async fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    match run(&arguments).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("netlab-rendezvous: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(arguments: &[String]) -> Result<(), String> {
    let options = parse(arguments)?;

    // The DHT first, and it is not arbitrary. Its nodes bootstrap off each
    // other synchronously, so by the time this returns the network exists; the
    // relay only has to be listening. Starting the relay first would mean
    // reporting `ready` before the DHT could answer a query.
    let testnet = Testnet::builder(options.dht_nodes)
        .bind_address(options.bind)
        .build()
        .map_err(|error| format!("could not start {} DHT nodes: {error}", options.dht_nodes))?;

    let mut relay = RelayConfig::new(SocketAddr::from((options.bind, options.relay_port)));
    // Plain HTTP for the relay itself. Address discovery brings its own
    // certificate below rather than borrowing one from here.
    relay.tls = None;

    // Address discovery, on the one port the client will look at. Without it
    // two NAT'd peers never learn a translated address and no punch is even
    // attempted — see the module note.
    let mut quic = QuicConfig::new(SocketAddr::from((options.bind, QAD_PORT)));
    quic.server_config = Some(self_signed_for(options.bind)?);

    // Built from `default` and mutated because `ServerConfig` is
    // `#[non_exhaustive]`, so a struct literal will not compile from outside
    // its crate. What is not set here — the metrics endpoint — defaults to off.
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    config.quic = Some(quic);

    let server = Server::spawn(config)
        .await
        .map_err(|error| format!("could not start the relay: {error}"))?;

    let address = server
        .http_addr()
        .ok_or("the relay started but reported no HTTP address")?;

    // Reported so a run that silently lost address discovery is visible in the
    // log rather than only in a topology that stopped punching.
    let discovery = match server.quic_addr() {
        Some(addr) => addr.to_string(),
        None => "none — NAT topologies cannot punch without it".to_string(),
    };

    // Reported rather than assumed, because an ephemeral port is the default
    // and a fixed one can still be refused.
    println!("netlab-rendezvous: relay=http://{address}");
    println!(
        "netlab-rendezvous: bootstrap={}",
        testnet.bootstrap.join(",")
    );
    println!("netlab-rendezvous: address-discovery={discovery}");
    println!("netlab-rendezvous: ready");

    // Flushed explicitly. A harness is blocked reading these lines, and stdout
    // to a pipe is block-buffered — without this the lab would wait for a
    // process that had already done its job and was sitting on the buffer.
    use std::io::Write;
    std::io::stdout()
        .flush()
        .map_err(|error| format!("could not flush the report: {error}"))?;

    // Nothing to do but stay alive and hold both services. The lab kills the
    // process group when it tears the topology down; SIGINT is handled so that
    // running this by hand behaves.
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("could not wait for a signal: {error}"))?;

    drop(testnet);
    Ok(())
}
