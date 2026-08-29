# iroh 1.0.3 and pkarr API survey

Date: **2026-08-24**
Status: **reference for
[`../plans/peer-to-peer-transport-plan-2026-08-20.md`](../plans/peer-to-peer-transport-plan-2026-08-20.md)
phases 2 and 3**

An exploratory report, not a contract. It records what the crates looked like on
the date above so the next session does not re-derive it, and it is kept because
recalled knowledge of iroh **does not compile**: the 1.0 release renamed
`NodeId` to `EndpointId`, `NodeAddr` to `EndpointAddr`, and `iroh::discovery` to
`iroh::address_lookup`, with no deprecated aliases.

Produced by four parallel research agents against docs.rs and the crate sources,
then synthesized. Verification is uneven and the report says so inline; treat
the levels as they are labelled:

- **compiled and ran** — strongest. The connect/accept/stream path and the whole
  pkarr publish-and-resolve round trip were both executed.
- **compiled** — the code typechecks but was never run.
- **read from docs** — weakest, and the source of the disagreements flagged in
  section 6.

**Section 6 is the part to read first when picking this up.** It separates what
a `cargo build` settles in seconds from what needs two peers, and from the three
things that can only be learned on a real network — including the one that
matters most: nobody has observed genuine NAT hole punching, because both live
runs had their endpoints on a single host. That is the feature's whole value
proposition and it remains unproven here.

Two findings changed the design rather than merely informing it, and both are
carried into the plan:

- **iroh 1.0.3 has no DHT support at all**, so `pkarr` is used directly for the
  mainline DHT that entry 10 specifies.
- **iroh's own `EndpointInfo::to_pkarr_signed_packet` silently corrupts the
  identity** under a derived signing key — it discards the endpoint id and the
  reader recovers the *signing* key as the peer's id, so the QUIC handshake then
  fails peer verification. Proven by execution, not inferred.

---

# Implementation brief — iroh 1.0.3 QUIC transport + pkarr rendezvous for Drop

Synthesized from four research reports. Verification status differs sharply between them and I flag it inline: **compiled+ran** > **compiled** > **read from docs.rs**. Where reports disagree, the disagreement is named rather than resolved.

---

## 0. What the repo already gives you (checked in the working tree, not from the reports)

| Fact | Location |
|---|---|
| `Transport` trait, `Frame`, `TransportError` | `/home/opq/projects/drop/cli/src/transport/mod.rs` |
| **Framing already exists and already implements `Transport`** | `/home/opq/projects/drop/cli/src/transport/framed.rs` — `FramedTransport<R, W>` over tokio `AsyncRead`/`AsyncWrite` |
| `MAX_FRAME_BYTES = CHUNK_PLAINTEXT_BYTES + TAG_BYTES` = `1024*1024 + 16` = **1 048 592** | `crypto/src/envelope.rs:24,28` |
| **HKDF derivation is done**: `rendezvous_secret(&TransferCode) -> [u8; 32]`, `Hkdf::<Sha256>::new(None, code.nameplate().as_bytes()).expand(b"drop/v1/rendezvous", …)` | `crypto/src/rendezvous.rs`, exported as `drop_crypto::rendezvous_secret` |
| The sender speaks first: `send_transfer` calls `await_peer()` then immediately `exchange_keys` → `send_control({"type":"key_exchange"})`. `receive_transfer` opens with `while let Some(frame) = transport.receive()` and writes nothing first. | `cli/src/transport/../send.rs:111-170`, `recv.rs:178` |
| Recorded decision that a direct transport answers `await_peer` **immediately** | `docs/decisions.md` entry 12 + the doc comment on `Transport::await_peer` |
| Nameplate today comes from the relay over HTTP (`client::create_session`) | `cli/src/client.rs:75` |
| `Cargo.lock` already contains `ring 0.17.14`, `rustls 0.23.43`, `tokio 1.53.1`, duplicated `sha2` 0.10/0.11 and `hkdf` 0.12/0.13 | verified by grep |

Consequences: **write no new framing code**, **write no new HKDF code**, and adding iroh introduces **no new C toolchain requirement** (ring is already in the lock via `ureq`/`tokio-tungstenite`) and no rustls version conflict — iroh 1.0.3 resolves to exactly the 0.23.43 already present.

---

## 1. Exact Cargo.toml lines

### `cli/Cargo.toml`

```toml
[package]
# ...
rust-version = "1.91"   # WAS "1.85". Forced: iroh 1.0.3 declares rust-version = "1.91".

[dependencies]
# QUIC transport. Default features (metrics, fast-apple-datapath, portmapper,
# tls-ring) are the ones you want: presets::N0 / Minimal only exist when
# tls-ring or tls-aws-lc-rs is on, and tls-ring is a default. ring 0.17.14 is
# already in Cargo.lock, so this adds no new C build requirement.
iroh = "1.0.3"

# The compact string form of an EndpointAddr, for stuffing into one TXT record.
iroh-tickets = "1.0.0"

# Rendezvous over the mainline DHT. `dht` implies the client machinery
# (dht = ["dep:mainline", "__client"]). default-features = false drops
# `relays`, which is the only thing that drags in reqwest.
pkarr = { version = "8.0.0", default-features = false, features = ["dht"] }
```

Nothing else. No `hkdf`/`sha2` — `drop-crypto` already owns the derivation.

**Do not add**, each for a specific reason:
- `noq` — iroh re-exports its `SendStream`/`RecvStream`; a direct dependency at a different version gives you two incompatible `SendStream` types.
- `quinn` — gone. iroh 1.0 uses n0's fork `noq` 1.1.x.
- `iroh-dns` — only needed for `iroh_dns::pkarr::SignedPacket`, which §5 says not to use.
- `rustls` — only needed to construct `PkarrRelayClient`, which §5 says not to use.

Leave `rust-version = "1.85"` alone in the **root** `Cargo.toml` and in **`crypto/Cargo.toml`**: neither depends on iroh, and `drop-crypto` also compiles to wasm. Only `cli` must move. CI uses `dtolnay/rust-toolchain@stable` in both `ci.yml` and `release.yml`, so no workflow pin needs changing — but anyone with a pinned local 1.85 will be hard-stopped (report 1 verified the rejection empirically: *"rustc 1.85.0 is not supported by the following packages: iroh@1.0.3 requires rustc 1.91, … noq@1.1.1 requires rustc 1.88"*).

Optional trim, **not** recommended for the first cut: `iroh = { version = "1.0.3", default-features = false, features = ["tls-ring"] }` drops `metrics` and `portmapper`; `portmapper` does UPnP/NAT-PMP, so dropping it likely lowers direct-connection success behind consumer routers — the opposite of what this feature is for.

---

## 2. Wire framing for (a) — it is already written; reuse it

`FramedTransport<R, W>` in `cli/src/transport/framed.rs` is the answer, verbatim. It was deliberately built ahead of the connection so the framing is testable over an in-memory pipe (`a_whole_transfer_crosses_a_bare_byte_pipe` already moves a whole 2 MiB transfer through it with no socket).

```text
┌──────┬─────────────────┬────────────────────────────┐
│ kind │ length (BE u32) │ payload                    │
│ 1 B  │ 4 B             │ `length` bytes             │
└──────┴─────────────────┴────────────────────────────┘
  0x01  control — payload is UTF-8 JSON (serde_json::Value)
  0x02  chunk   — payload is sealed bytes, opaque to the transport
```

- **Header:** 5 bytes, no alignment, no magic, no version field (the ALPN carries the version — see §3).
- **Distinguishing control from chunk:** the leading kind byte. Any other value is `TransportError::Malformed`, not ignored.
- **Length encoding:** big-endian `u32`, payload length only (header excluded).
- **Max sizes:** `MAX_FRAME_BYTES = 1 048 592` (one sealed chunk: 1 MiB plaintext + 16-byte AES-GCM tag). Largest on-wire frame = **1 048 597 bytes**. The cap is checked **before** the payload allocation, because the length is an allocation request from a peer nobody has authenticated yet. Control frames are far smaller (biggest is hex-encoded sealed metadata, relay-capped at 8 KiB), so one ceiling covers both and the kind byte never has to be trusted first.
- **Header and payload go out in one `write_all`**, then `flush()`. On `SendStream`, `poll_flush` is a documented no-op, so the flush is free.
- **Clean end vs truncation:** `read_header` loops on `read()` counting bytes. Zero bytes at `filled == 0` is the peer finishing (`Ok(None)`); zero bytes at `filled > 0` is `Malformed("the stream ended N bytes into a 5 byte frame header")`. This is the one behaviour that depends on an unverified claim — see §6, item B4.

**Why this survives QUIC.** A QUIC bidirectional stream is an ordered, reliable byte stream with **no message boundaries**. iroh's own docs say it twice, verbatim, on `read_chunk` and `read_many_chunks`: *"Chunk boundaries do not correspond to peer writes, and hence cannot be used as framing."* Ordering **within** one stream is guaranteed; across streams it is not. Use exactly one bidi stream per transfer.

Both reports that examined the streams (2 and 4, plus report 1 which compiled it) agree that `SendStream: tokio::io::AsyncWrite` and `RecvStream: tokio::io::AsyncRead` **unconditionally** — tokio's traits, not futures-io (the docs.rs links resolve to `tokio::io::async_read::AsyncRead` and `tokio::io::read_buf::ReadBuf`; the futures-io impls are gated behind a `noq` feature). So:

```rust
FramedTransport::new(recv, send)   // reader FIRST, writer SECOND
```

**Tuple-order trap:** `open_bi()` and `accept_bi()` both return `(SendStream, RecvStream)` — the reverse of `FramedTransport::new(reader, writer)`. Both orderings would typecheck at some call sites. Write `new(recv, send)`.

### Do not use QUIC datagrams
`Connection::max_datagram_size()` is *"a little over a kilobyte at minimum"*, and datagrams are unreliable and unordered. Streams are the only path for 1 MiB chunks.

---

## 3. The exact iroh calls

All signatures below are transcribed from crate sources (not rendered docs) by report 1 and cross-checked by reports 2 and 4. Report 1 additionally **compiled and ran** the connect/accept/stream path end to end on iroh 1.0.3 / rustc 1.91.0.

### 3.0 Rename warning — recalled pre-1.0 knowledge will not compile

`NodeId` → `EndpointId`, `NodeAddr` → `EndpointAddr`, `NodeInfo` → `EndpointInfo`, `iroh::discovery` → `iroh::address_lookup`, `Discovery` → `AddressLookup`, `StaticProvider` → `MemoryLookup`, `Endpoint::node_id()` → `Endpoint::id()`, `Endpoint::node_addr()` → `Endpoint::addr()`, `Endpoint::add_node_addr()` → **deleted**. There are no deprecated aliases. `https://docs.rs/iroh/1.0.3/iroh/discovery/` is a hard 404. The task text says "NodeAddr"; the type is `EndpointAddr`.

### 3.1 Build endpoint

```rust
pub fn builder(preset: impl Preset) -> Builder                       // iroh::Endpoint
pub fn alpns(mut self, alpn_protocols: Vec<Vec<u8>>) -> Self         // iroh::endpoint::Builder
pub fn relay_mode(mut self, relay_mode: RelayMode) -> Self
pub fn secret_key(mut self, secret_key: SecretKey) -> Self
pub fn addr_filter(mut self, filter: AddrFilter) -> Self
pub async fn bind(self) -> Result<Endpoint, BindError>
pub async fn bind(preset: impl Preset) -> Result<Self, BindError>    // Endpoint::bind shorthand; cannot set ALPNs, so dial-only
```

```rust
use iroh::{Endpoint, RelayMode, endpoint::presets};

pub const DROP_ALPN: &[u8] = b"drop/transfer/1";   // bump the integer on any framing change

let endpoint = Endpoint::builder(presets::Minimal)
    .relay_mode(RelayMode::Default)          // Minimal implies RelayMode::Disabled — put relays back
    .alpns(vec![DROP_ALPN.to_vec()])         // required to ACCEPT; harmless on the dialer
    .bind()
    .await?;
```

**Preset choice — a real tradeoff, and the reports were verified on different sides of it.**
- `presets::N0` = crypto provider + n0 production relays + `PkarrPublisher`/`PkarrResolver` against n0's DNS + DNS lookup. **This is the only configuration in which anyone actually established a live iroh connection** (report 1: relay-mediated transfer, LAN direct dial, bare-`EndpointId` dial in 248 ms).
- `presets::Minimal` = crypto provider only, **no address lookup services and `RelayMode::Disabled`** (report 4, compiled but never connected). Drop does its own publishing to the mainline DHT, so iroh's n0-DNS publisher is redundant *and* it puts a second copy of your addresses on a third-party server keyed by your endpoint id.

**Recommendation:** target `Minimal` + `.relay_mode(RelayMode::Default)`; if the first two-peer test fails, switch to `presets::N0` to isolate whether the preset is the cause, since that is the configuration with a known-good live run. Never ship `Minimal` without restoring `relay_mode` — with no relay there is no home relay, no hole-punching assistance, and `online()` **pends forever**.

**Do not pass the rendezvous key as the endpoint secret key.** `rendezvous_secret()` and `SecretKey::from_bytes` are both 32-byte ed25519 seeds and it is tempting. Anyone who guesses the nameplate would then hold the sender's endpoint private key and could complete the QUIC handshake as the sender. Omit `.secret_key()` entirely: report 1 states that an unset key means a fresh random key is generated on every bind, which is exactly the per-transfer unlinkability you want, and it sidesteps the `SecretKey::generate()` signature disagreement in §6.

### 3.2 Get own EndpointAddr

```rust
pub fn id(&self) -> EndpointId                                       // immediate after bind, no network
pub fn addr(&self) -> EndpointAddr                                   // snapshot of watch_addr().get()
pub fn watch_addr(&self) -> impl n0_watcher::Watcher<Value = EndpointAddr> + use<>
pub async fn online(&self)                                           // NO timeout, NO return value
```

```rust
use std::time::Duration;

// bind() returning does NOT mean you are reachable: right after bind, addr()
// has local IPs and an EMPTY relay-URL list. online() waits for the home-relay
// handshake. Measured 3.2 s against n0 production relays.
tokio::time::timeout(Duration::from_secs(15), endpoint.online())
    .await
    .map_err(|_| TransportError::Connect("no relay answered in 15s".into()))?;

let addr: EndpointAddr = endpoint.addr();
```

Compile trap: `iroh::NET_REPORT_TIMEOUT` is a bare **`u64` equal to 5 (seconds)**, not a `Duration`. It also budgets too tightly against a measured 3.2 s. Use a literal 10–15 s.

`watch_addr().initialized()` **does not compile** — `initialized()` needs `n0_watcher::Nullable`, implemented only for `Option<T>`/`Vec<T>`. If you prefer watching to `online()`, loop on `watcher.get()` / `watcher.updated().await` testing `!addr.is_empty()`, and `use iroh::Watcher;` must be in scope for `.get()`.

```rust
pub struct EndpointAddr { pub id: EndpointId, pub addrs: BTreeSet<TransportAddr> }
#[non_exhaustive] pub enum TransportAddr { Relay(RelayUrl), Ip(SocketAddr), Custom(CustomAddr) }
pub type EndpointId = PublicKey;    // a plain alias — a derived ed25519 public key IS an EndpointId
```
`EndpointAddr` derives `Serialize`/`Deserialize` unconditionally. `TransportAddr` is `#[non_exhaustive]`; every match needs a wildcard arm.

**Privacy, and it is a product decision, not a transport detail.** A serialized `EndpointAddr` leaks every local interface address. Report 1's real run produced 297 bytes of JSON containing `10.107.49.170`, `10.157.216.14`, and Docker bridges `172.17.0.1`/`172.18.0.1`/`172.19.0.1` alongside the public IP. Drop publishes this under a **24-bit, enumerable** nameplate — `docs/decisions.md` entry 10 already names the address disclosure, but it named the public address, not the LAN topology. `AddrFilter` only offers `unfiltered()`, `relay_only()`, `ip_only()`, so there is no built-in "public IPs only": filter the `BTreeSet<TransportAddr>` yourself before building the ticket, or accept it deliberately. Note that stripping RFC1918 addresses also kills LAN-local direct transfers — escalate rather than silently choosing.

### 3.3 Connect (dialer = the receiver)

```rust
pub async fn connect(&self, endpoint_addr: impl Into<EndpointAddr>, alpn: &[u8])
    -> Result<Connection, ConnectError>
```
`impl Into<EndpointAddr>` also accepts a bare `EndpointId` (via `From<EndpointId>`), which only works if a lookup service is configured — irrelevant here, we pass a fully-populated address. `ep.connect(own_id, ..)` fails with `ConnectWithOptsError::SelfConnect`.

### 3.4 Accept (acceptor = the sender)

```rust
pub fn accept(&self) -> Accept<'_>                        // Future<Output = Option<Incoming>>; None once closed
pub fn accept(self) -> Result<Accepting, ConnectionError> // iroh::endpoint::Incoming — SYNCHRONOUS
pub async fn alpn(&mut self) -> Result<Vec<u8>, AlpnError>// iroh::endpoint::Accepting — takes &mut self
impl Future for Accepting { type Output = Result<Connection, ConnectingError>; }
impl IntoFuture for Incoming { type Output = Result<Connection, ConnectingError>; }
pub fn remote_id(&self) -> EndpointId                     // on Connection; infallible on HandshakeCompleted
pub fn refuse(self)                                       // on Incoming
```
Two awaits, not one: `ep.accept().await` then `.await` on the `Incoming`/`Accepting`. Since we bind with a single ALPN, iroh rejects mismatches itself and the `Accepting::alpn()` inspection is optional; `let conn = ep.accept().await.ok_or(...)?.await?;` is the short form. `Accepting` has **no** `remote_id()` — peer identity is only on `Connection`.

After the first accepted connection, `refuse()` every further `Incoming` (or stop accepting): `decisions.md` entry 10 requires that a failed attempt consumes the transfer, and a second acceptor would hand a grinding attacker a retry.

### 3.5 open_bi / accept_bi — **and who calls which**

```rust
pub fn open_bi(&self)   -> OpenBi<'_>    // Future<Output = Result<(SendStream, RecvStream), ConnectionError>>
pub fn accept_bi(&self) -> AcceptBi<'_>  // Future<Output = Result<(SendStream, RecvStream), ConnectionError>>
```

**This is the sharpest orchestration decision in the whole brief.** `accept_bi()` does not resolve when the peer *opens* a stream — only when the peer first **writes** on it. iroh's own doc: *"Application protocols should always arrange for the endpoint which will first transmit on a stream to be the endpoint responsible for opening it."*

Drop's protocol has the **sender** speak first (`exchange_keys` sends `key_exchange` before anything is read; `receive_transfer` writes nothing until it has received). Phase 3 has the **receiver** dial (it is the one that resolved an address). So the connection dialer and the first speaker are **different sides**.

QUIC permits either endpoint to open a bidirectional stream, so resolve it this way:

| | QUIC connection | QUIC stream | first byte |
|---|---|---|---|
| **Sender** | accepts (`Endpoint::accept`) | **opens** (`open_bi`) | writes `key_exchange` |
| **Receiver** | dials (`Endpoint::connect`) | **accepts** (`accept_bi`) | reads |

This needs **no handshake preamble** and no changes to `send.rs`/`recv.rs`. If you instead let the dialer open the stream, both sides park forever — the sender in `accept_bi`, the receiver in `receive()`.

### 3.6 read / write / finish / close

```rust
// SendStream
pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), WriteError>    // NOT cancel-safe
pub async fn write(&mut self, buf: &[u8]) -> Result<usize, WriteError>     // cancel-safe
pub fn finish(&mut self) -> Result<(), ClosedStream>                       // SYNCHRONOUS — no .await
pub fn stopped(&self) -> Stopped   // Future<Output = Result<Option<VarInt>, StoppedError>>
pub fn reset(&mut self, error_code: VarInt) -> Result<(), ClosedStream>

// RecvStream
pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ReadError>       // Ok(None) = clean EOF
pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadExactError>       // NOT cancel-safe
pub fn stop(&mut self, error_code: VarInt) -> Result<(), ClosedStream>

// Connection / Endpoint
pub fn close(&self, error_code: VarInt, reason: &[u8])   // SYNCHRONOUS, infallible. VarInt: 0u32.into()
pub async fn closed(&self) -> ConnectionError
pub async fn close(&self)                                // Endpoint::close
```

You call none of these directly for data — `FramedTransport` goes through the tokio traits. What you do call is the close sequence. `AsyncWriteExt::shutdown()` on `SendStream` is literally `Poll::Ready(self.get_mut().finish().map_err(Into::into))`, so `FramedTransport::close()` (which shuts the writer down) **is** the correct `finish()`. The connection and endpoint still have to be closed separately:

```rust
async fn close(&mut self) {
    self.framed.close().await;                       // == SendStream::finish()
    self.connection.close(0u32.into(), b"complete"); // synchronous
    self.endpoint.close().await;                     // lets CONNECTION_CLOSE actually reach the peer
}
```
Order matters: *"Once the local side sends a CONNECTION_CLOSE frame … the remote endpoint may drop any data it received but is as yet undelivered to the application, including data that was acknowledged."* Drop is safe here because `send_transfer` already awaits the receiver's `complete` frame in `await_completion` before calling `close()` — an application-level ack. Do not reorder that. If you ever close without that ack, await `stopped()` first (`Ok(None)` once the peer has acked all stream data).

Note also: **dropping** a `SendStream` implicitly `finish()`es, dropping a `RecvStream` implicitly `stop(0)`s, and dropping every `Connection` handle auto-closes with error code 0. So an early `?` return silently looks like a *clean* finish to the peer. Error paths should close explicitly with a **non-zero** code so a truncated transfer is distinguishable from a completed one.

### 3.7 Error mapping into `TransportError`

iroh 1.0 uses `n0-error`; `?` does **not** convert `ConnectionError`/`WriteError`/`ReadExactError`/`ClosedStream` into anything of Drop's. Every conversion is an explicit `.map_err(...)`, which is what you want anyway:

- `ConnectError`, `BindError`, `ConnectingError` → `TransportError::Connect`
- `ConnectionError::ApplicationClosed(_)` = the peer called `close()` — **routine**, not a failure. `LocallyClosed` = we did.
- `ConnectionError::{TimedOut, Reset, TransportError, ConnectionClosed, VersionMismatch, CidsExhausted}` → `TransportError::Io`
- `WriteError::Stopped(code)` = the receiver called `stop()`, i.e. "I don't want the rest" — an application decision. Worth its own message rather than being folded into `Io`.
- `ReadError` has **no** clean-EOF variant; clean EOF is only ever `Ok(None)` / a 0-byte read.
- `ConnectError`, `ConnectWithOptsError`, `BindError`, `TransportAddr` are all `#[non_exhaustive]`.

### 3.8 Sketch: `cli/src/transport/quic.rs`

```rust
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::{Connection, RecvStream, SendStream},
};
use serde_json::Value;

use super::{Frame, Transport, TransportError, framed::FramedTransport};

pub const DROP_ALPN: &[u8] = b"drop/transfer/1";

pub struct QuicTransport {
    framed: FramedTransport<RecvStream, SendStream>,
    connection: Connection,
    endpoint: Endpoint,
}

impl QuicTransport {
    /// Sender side. Blocks until the receiver dials and the stream is open, so
    /// `await_peer` can keep the trait's default `Ok(())` — see decisions.md 12.
    pub async fn accept(endpoint: Endpoint) -> Result<Self, TransportError> {
        let incoming = endpoint.accept().await.ok_or_else(|| {
            TransportError::Connect("the endpoint closed before a peer arrived".into())
        })?;
        let connection = incoming.await.map_err(|e| TransportError::Connect(e.to_string()))?;
        // The sender speaks first, so the sender opens the stream — even though
        // it accepted the connection. accept_bi does not resolve until a write.
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        Ok(Self { framed: FramedTransport::new(recv, send), connection, endpoint })
    }

    /// Receiver side. `accept_bi` resolves on the sender's first frame.
    pub async fn connect(endpoint: Endpoint, addr: EndpointAddr) -> Result<Self, TransportError> {
        let connection = endpoint
            .connect(addr, DROP_ALPN)
            .await
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        let (send, recv) = connection
            .accept_bi()
            .await
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        Ok(Self { framed: FramedTransport::new(recv, send), connection, endpoint })
    }
}

impl Transport for QuicTransport {
    // await_peer: keep the default.
    async fn send_control(&mut self, frame: Value) -> Result<(), TransportError> {
        self.framed.send_control(frame).await
    }
    async fn send_chunk(&mut self, chunk: Vec<u8>) -> Result<(), TransportError> {
        self.framed.send_chunk(chunk).await
    }
    async fn receive(&mut self) -> Result<Option<Frame>, TransportError> {
        self.framed.receive().await
    }
    async fn close(&mut self) {
        self.framed.close().await;
        self.connection.close(0u32.into(), b"complete");
        self.endpoint.close().await;
    }
}
```

`SendStream`/`RecvStream` are `Send + Sync + Unpin`, satisfying `FramedTransport`'s bounds and the trait's `+ Send` futures.

**Keeping `await_peer` as the default is a deliberate choice** to honour `decisions.md` entry 12 and the doc comment on the trait ("a direct transport answers immediately"). It means the *constructor* is where the sender waits, so the CLI's "waiting for the receiver" message belongs there, and `send.rs`'s existing `eprintln!("Receiver connected.")` after `await_peer()` stays true. If you prefer to move the wait into `await_peer` (better UX hook, single place to bound the wait for Phase 4 fallback), that is defensible — but then you must amend both `docs/decisions.md` entry 12 and the `await_peer` doc comment, because they currently state the opposite.

### 3.9 Flow control — the sender's 16 MiB window is a fiction today

`send.rs` allows `WINDOW_BYTES = 16 MiB` in flight before waiting for acks. noq-proto's defaults are `stream_receive_window = 1_250_000`, `send_window = 10_000_000`, `receive_window = VarInt::MAX`, `max_concurrent_bidi_streams = 100`, `max_idle_timeout = 30_000 ms`. One sealed frame is 1 048 597 bytes, so **exactly one chunk fits the per-stream window** and the second parks on flow control until the receiver reads. That is backpressure, not an error — but it means QUIC, not Drop, sets throughput, and a receiver that stops reading hangs the sender rather than erroring it. Raise `stream_receive_window` to ≥ 16 MiB (and `send_window` with it) via `QuicTransportConfigBuilder::stream_receive_window(value: VarInt)`. See §6 A3: how that builder attaches to `Endpoint::builder` is **unverified**.

---

## 4. The exact pkarr calls

Report 3 **compiled and ran** all of this against the live mainline DHT: nameplate → HKDF → keypair → publish an `EndpointAddr` as a 135-byte `EndpointTicket` (322-byte signed packet) → resolve from a separate process → identical `EndpointId`.

```rust
// keypair from bytes — ANSWER: takes the HKDF output directly, infallibly.
pub fn from_secret_key(secret_key: &SecretKey) -> Keypair       // pkarr::Keypair
//   `SecretKey` here is ed25519_dalek::SecretKey, and in ed25519-dalek 3.0.0
//   that is `pub type SecretKey = [u8; 32]`. Any 32 bytes are a valid seed.
pub fn public_key(&self) -> PublicKey
pub fn to_z32(&self) -> String                                  // 52 chars

// build a record
pub fn builder() -> SignedPacketBuilder                          // pkarr::SignedPacket
pub fn txt(self, name: Name<'_>, text: TXT<'_>, ttl: u32) -> Self
pub fn sign(self, keypair: &Keypair) -> Result<SignedPacket, SignedPacketBuildError>
pub const MAX_BYTES: u64 = 1104

// client
pub fn builder() -> ClientBuilder
pub fn minimum_ttl(&mut self, ttl: u32) -> &mut Self             // NOTE &mut self -> &mut Self
pub fn build(&self) -> Result<Client, BuildError>

// publish / resolve
pub async fn publish(&self, packet: &SignedPacket) -> Result<StoredNodeCount, PublishError>
//   pub type StoredNodeCount = u32 — DHT nodes that acked. Measured 28-82.
pub async fn resolve(&self, key: &PublicKey, policy: ResolvePolicy) -> Result<SignedPacket, ResolveError>
pub enum ResolvePolicy { CacheOnly, CacheFirst, NetworkOnly }

// reading it back
pub fn resource_records(&self, name: &str) -> impl Iterator<Item = &ResourceRecord<'_>>
```

### Sketch: `cli/src/transport/rendezvous.rs`

```rust
use std::str::FromStr;

use drop_crypto::{TransferCode, rendezvous_secret};
use iroh::EndpointAddr;
use iroh_tickets::{Ticket, endpoint::EndpointTicket};
use pkarr::{Client, ClientBuilder, Keypair, ResolvePolicy, SignedPacket};

const TXT_NAME: &str = "_drop";
const TTL: u32 = 60;

/// Both peers derive the identical keypair. The seed already exists in
/// drop-crypto and is normalised, so a lowercase retype meets in the same place.
fn meeting_keypair(code: &TransferCode) -> Keypair {
    Keypair::from_secret_key(&rendezvous_secret(code))
}

/// One client per process, kept alive and cloned. The first operation pays the
/// DHT bootstrap cost (3-5 s); after that a republish is 0.6-0.8 s.
/// `minimum_ttl(0)` defeats pkarr's 5-minute cache floor, which would otherwise
/// serve a receiver a stale record for 300 s after the sender republishes.
pub fn dht_client() -> Result<Client, pkarr::errors::BuildError> {
    let mut builder = ClientBuilder::default();   // NB: &mut self builder — cannot one-line the chain
    builder.minimum_ttl(0);
    builder.build()
}

/// SENDER. Returns the number of DHT nodes that stored it.
pub async fn publish(client: &Client, code: &TransferCode, addr: &EndpointAddr) -> Result<u32, Error> {
    let ticket = EndpointTicket::from(addr.clone()).encode_string();   // ~135 bytes
    let packet = SignedPacket::builder()
        .txt(TXT_NAME.try_into()?, ticket.as_str().try_into()?, TTL)
        .sign(&meeting_keypair(code))?;
    Ok(client.publish(&packet).await?)
}

/// RECEIVER. Call in a loop: a miss costs 2-3 s and returns Err, so the DHT
/// timeout IS the poll interval — no sleep.
pub async fn resolve(client: &Client, code: &TransferCode) -> Result<EndpointAddr, Error> {
    let key = meeting_keypair(code).public_key();
    let packet = client.resolve(&key, ResolvePolicy::CacheFirst).await?;
    let record = packet.resource_records(TXT_NAME).next().ok_or("no _drop TXT record")?;
    let pkarr::dns::rdata::RData::TXT(txt) = &record.rdata else {
        return Err("the record was not TXT".into());
    };
    let ticket: String = txt.clone().try_into()?;
    Ok(EndpointTicket::from_str(&ticket)?.into())
}
```

`EndpointTicket` is the recommended carrier, but not the only one: `EndpointAddr` is `Serialize`/`Deserialize`, and report 1 measured a 5-address `serde_json` form at 297 bytes — comfortably inside the ~914-byte usable TXT budget under a `_drop` name (920 at the apex `.`). Dropping `iroh-tickets` in favour of `serde_json` is a defensible way to shed a dependency; the ticket is more compact and canonical. Values over 255 bytes are transparently split into multiple DNS character-strings by `try_into()`, so no manual chunking either way. Hard ceiling: `MAX_BYTES = 1104` (32 pubkey + 64 sig + 8 timestamp + ≤1000 DNS), and over it you get `SignedPacketBuildError::PacketTooLarge`.

### Measured timings (report 3, one host, live DHT, small samples)

| Operation | Measured |
|---|---|
| Publish, cold client (bootstrap included) | 3.29 / 4.05 / 4.40 / 4.62 / 5.05 s |
| Republish, warm client | 0.60–0.83 s |
| Resolve `CacheFirst`, cold separate process | 0.15 / 0.19 / 0.30 / 0.36 / 0.42 / 0.60 s |
| Resolve `NetworkOnly` | 2.3–3.3 s |
| Resolve, record absent | 2.1–3.1 s per attempt |

Sequencing that follows directly (and satisfies the plan's "must not print 'waiting' before the record is actually retrievable"): **bind → `online()` → publish → only then print the code / say waiting → accept**.

Republish while waiting: the DNS TTL in the record has nothing to do with DHT retention — it only drives pkarr's client-side cache. DHT retention is set by the storing nodes (libtorrent convention ~2 hours) and is **not** something either crate defines. For a seconds-to-minutes rendezvous one publish suffices; for a long wait, republish every 15–30 min on the warm client at 0.6–0.8 s each, which also refreshes the node set as nodes churn.

### Security properties to preserve

`crypto/src/rendezvous.rs` already documents these; the transport must not weaken them:
- The derived key is **not a secret**. Anyone who guesses the nameplate derives the identical *private* half, so a resolved address is **not proof of who published it**. Authentication is SPAKE2's job and only SPAKE2's job — do not treat `Connection::remote_id()` as an identity check.
- Only the **sender** ever publishes. Both sides can, so a receiver that publishes would clobber the record.
- Nothing confidential goes in the record. It is world-readable on a public DHT under a 24-bit key.

---

## 5. Can iroh's built-in discovery carry the derived-key scheme? — **No. Use raw pkarr plus a fully-populated `EndpointAddr`.**

Three independent reasons, in descending order of finality:

1. **iroh 1.0.3 has no DHT support at all.** No `DhtDiscovery`, no mainline integration, no `dht` feature. The complete 1.0.3 feature list is `metrics, fast-apple-datapath, portmapper, tls-ring, tls-aws-lc-rs, platform-verifier, qlog, test-utils, unstable-custom-transports, unstable-net-report`. iroh's pkarr records travel **only over an HTTP pkarr relay** (n0's `dns.iroh.link`). `docs/decisions.md` entry 10 specifies the mainline DHT. Nothing in iroh reaches it.

2. **No registered lookup service publishes under a key you choose.** `Builder::address_lookup(...)` signs with the endpoint's own secret key. The lower primitives *do* accept an arbitrary key — `EndpointInfo::to_pkarr_signed_packet(&derived_secret, ttl)`, `PkarrPublisher::n0_dns().build(derived_secret, tls)`, `PkarrRelayClient::publish(&packet)` which PUTs to `<relay>/<z32-of-signing-key>` — so a derived-key scheme *is* expressible against n0's relay. It is just the wrong backing store, and it makes Drop's rendezvous depend on a third party's free production service whose acceptance of non-endpoint keys **was never tested** (report 4 deliberately did not write to it).

3. **The iroh helper that looks like it does what you want silently corrupts the identity.** Report 4 proved this by execution: `EndpointInfo::to_pkarr_signed_packet` discards `self.endpoint_id` (it serializes only `to_txt_strings()`, measured as `["relay=…", "addr=…", "user-data=…"]` — no id entry), and `from_pkarr_signed_packet` does `let endpoint_id = EndpointId::from_bytes(packet.public_key().as_bytes())`. Under a nameplate-derived signing key the receiver therefore recovers **the nameplate key as the peer's endpoint id**, and the QUIC handshake fails TLS peer verification. If anyone reaches for this path anyway, the sender's real id must be smuggled through `UserData` (245-byte cap; a z32 id is 52 chars) or through the DNS name via `from_txt_lookup`.

**So:** publish and resolve with the `pkarr` crate (8.0.0, `dht`), carry the whole `EndpointAddr` as an `EndpointTicket` string, and hand iroh the finished address at `connect()`. This is supported with **zero** lookup services configured: iroh consults a lookup service only lazily — `path_state::resolve_remote` documents *"If there already is a known path, `Ok(())` is returned immediately. Otherwise an address lookup is performed."* A fully-populated `EndpointAddr` therefore bypasses discovery entirely. With zero services and no known path you would get `AddressLookupFailed::NoServiceConfigured`.

**Optional hardening (this is the "manual NodeAddr injection" API).** If you want iroh able to *re*-resolve the peer later (paths expire, a peer roams mid-transfer), register `MemoryLookup` — iroh 1.0's replacement for both `StaticProvider` and the deleted `Endpoint::add_node_addr`. It is `Clone` with interior mutability, so clone it into the builder and keep mutating after bind:

```rust
use iroh::address_lookup::memory::MemoryLookup;
let mem = MemoryLookup::new();
mem.add_endpoint_info(addr.clone());          // EndpointAddr: Into<EndpointInfo>
let endpoint = Endpoint::builder(presets::Minimal)
    .relay_mode(RelayMode::Default)
    .address_lookup(mem.clone())
    .bind().await?;
mem.set_endpoint_info(newer_addr);            // after a fresh DHT lookup
mem.remove_endpoint_info(id);                 // retract when the transfer ends
```
`Builder::address_lookup` pushes onto a `Vec` that starts empty, so this is purely additive and turns nothing else on.

**Two `SignedPacket` types exist and are not interchangeable.** `iroh_dns::pkarr::SignedPacket` (iroh's own, over simple-dns 0.11.3) vs `pkarr::SignedPacket` (simple-dns 0.12.0). Both land in the tree; `cargo tree -d` shows the duplicate. `Name`/`TXT`/`RData`/`ResourceRecord` from one will not typecheck against the other. The wire format is identical (`<32 pubkey><64 sig><8 BE timestamp><DNS packet>`), so you *could* bridge with bytes — but if you follow this brief you never touch iroh's type at all. `ed25519-dalek` unifies at 3.0.0.

---

## 6. Unresolved uncertainties

### A. Settled by `cargo build` — resolve these first, they are cheap

1. **MSRV.** iroh 1.0.3 requires rustc 1.91 (verified empirically by two reports). Confirm the toolchain in use, bump `cli/Cargo.toml` only, and confirm no contributor doc pins 1.85.
2. **`SecretKey::generate()` signature.** Report 1: `pub fn generate() -> Self` using `rand::random()` internally. Report 4: "some iroh versions take an RNG argument", not exercised. **Reports disagree.** Sidestepped by omitting `.secret_key()` entirely (unset ⇒ fresh random key per bind) — but confirm that omission actually produces a working acceptor.
3. **Transport-config plumbing.** `QuicTransportConfigBuilder::stream_receive_window(self, value: VarInt) -> Self` was read from docs (report 2). **How it attaches to `Endpoint::builder` is unverified by anyone** — presumably a `Builder::transport_config(...)`, name unconfirmed. Also unverified: whether iroh overrides noq-proto's defaults, so the 1 250 000-byte figure in §3.9 may not be the effective one. This is the difference between a 1 MiB and a 16 MiB in-flight window.
4. **`iroh-tickets` import path.** `iroh_tickets::{Ticket, endpoint::EndpointTicket}`, `EndpointTicket::from(addr).encode_string()`, `EndpointTicket::from_str(..)`. Compiled by report 3; not independently corroborated.
5. **`pkarr::dns` re-export paths** — `pkarr::dns::rdata::RData::TXT`, `Name`/`TXT` via `"_drop".try_into()?`. Compiled by report 3.
6. **`ClientBuilder` ergonomics** — takes `&mut self` and returns `&mut Self`, so `Client::builder().minimum_ttl(0).build()` does **not** chain. Bind a `let mut builder = ClientBuilder::default();` first.
7. **Whether `FramedTransport`'s trait bounds are actually satisfied** by `RecvStream`/`SendStream` in a real generic instantiation. Three reports agree the tokio impls are unconditional; nobody instantiated `FramedTransport<RecvStream, SendStream>`.
8. **Workspace dependency resolution.** Nobody compiled iroh **inside Drop**. I verified `Cargo.lock` already holds `ring 0.17.14`, `rustls 0.23.43`, `tokio 1.53.1` — exactly what iroh resolves to — so a conflict is unlikely, but `tokio-tungstenite 0.30` / `ureq 2` / `flate2` were never resolved against iroh's tree, and iroh pulls `reqwest 0.13`. Expect duplicated `simple-dns` (0.11.3 + 0.12.0) and possibly `sha2` (already duplicated 0.10/0.11 today).

### B. Settled only by a two-peer test, on one machine

1. **The whole orchestration in §3.5** — sender-accepts-connection-but-opens-stream, receiver-dials-but-accepts-stream. Reasoned from iroh's documented "first transmitter opens the stream" rule and from Drop's actual first-speaker; **never run in this configuration by anyone**. If `accept_bi`/`open_bi` deadlock, this is the first thing to look at.
2. **Whether the constructor-blocks design keeps `send.rs`'s "Receiver connected." honest**, and whether the receiver's constructor blocking in `accept_bi` until the sender's first frame produces acceptable UI latency.
3. **`Endpoint::online()` under `presets::Minimal + RelayMode::Default`.** Report 1 measured 3.2 s under `presets::N0`; report 4 never connected at all. If relays end up disabled, `online()` pends **forever** with no timeout and no return value — always wrap it.
4. **Clean EOF through the tokio `AsyncRead` impl.** `framed.rs::read_header` depends on a finished `RecvStream` surfacing as `Ok(0)`. Report 2 explicitly could not find the `impl AsyncRead for RecvStream` body and rests the claim on the `poll_read` doc comment plus convention. If it is wrong, the existing "peer finished cleanly" test passes over a pipe and fails over QUIC. Fallback: bypass the tokio trait and use the inherent `RecvStream::read_exact`, where `ReadExactError::FinishedEarly(0)` is clean EOF and `FinishedEarly(n>0)` is truncation — the same distinction `read_header` makes by hand.
5. **The full-size chunk path.** The largest payload anyone has moved over an iroh stream in any of this research is **5 bytes**. Backpressure at 1 048 597-byte frames, long-transfer stability, and the flow-control interaction in §3.9 are all unexamined.
6. **`Connection::close()` truncation** — that `await_completion`'s app-level ack really is sufficient before closing.

### C. Settled only on real networks — the product risks

1. **Genuine NAT hole punching was never observed.** Both reports that ran iroh had both endpoints on one host, so the "direct" path was loopback/LAN and the relay was exercised only as rendezvous. The relay-to-direct upgrade transition is unobserved. This is the feature's entire value proposition.
2. **Rendezvous → dial has never been demonstrated end to end.** Report 3 got a byte-identical `EndpointAddr` across processes, but its subsequent `connect()` timed out because its endpoint had only private 10.x/172.x addresses and no relay. Environment artifact, but nobody has closed the loop.
3. **DHT reachability on real networks.** All pkarr timings come from one sandbox with fast unfiltered outbound UDP (5 publishes, 8 resolves). Consumer NAT, mobile, and corporate networks will be slower or blocked outright. **mainline is IPv4-only** — `DhtConfig::bind_address`/`public_ip` are `Ipv4Addr`, bootstrap nodes are `SocketAddrV4` — so an IPv6-only host cannot rendezvous at all. That is a fallback case, not an error case.
4. **DHT retention time is undefined** by both pkarr and mainline; "republish hourly" is community guidance (pkarr README, pubky docs), never measured. Verify before relying on a rendezvous that outlives a few minutes.
5. **Two UDP sockets.** pkarr's DHT client runs its own node on its own port, entirely separate from iroh's endpoint socket — a second set of NAT mappings. Their interaction behind a restrictive NAT is untested, as is running a pkarr DHT node alongside iroh's own lookup services in one process.
6. **Cross-compilation to Drop's four release targets** is unproven and is the second-largest ship risk after the MSRV bump: iroh pulls `netwatch`, `portmapper`, `hickory-resolver`, `reqwest`. Also check binary size — this is a very large dependency addition for a tool that ships prebuilt binaries. `fast-apple-datapath` (a default) and the Windows socket paths were never built.
7. **n0's production relay infrastructure** is a third-party dependency for hole punching and for `online()`. No load, availability, or acceptable-use assessment exists.
8. **Concurrent publish under one nameplate** (`dht::PublishError::NotMostRecent`) was never exercised under real contention.

### D. Adjacent gaps this brief surfaces but does not solve

- **A p2p send has no nameplate source.** `client::create_session` allocates the nameplate over HTTP from the relay, so a serverless send must generate its own. A locally drawn 24-bit nameplate can also collide with a live DHT record — resolving first (2–3 s, and it returns `Err` on a miss) both checks for a collision and confirms DHT reachability before you print a code.
- **Cancel-safety is a future hazard.** `write_all`/`read_exact`/`read_to_end`/`write_chunk`/`write_all_chunks` are **not** cancel-safe; a dropped `write_all` may have written a partial prefix, silently corrupting the framing. `write`, `read`, `read_chunk`, `read_many_chunks`, `write_many_chunks` are. I grepped `send.rs`, `recv.rs`, `relay.rs`, `main.rs`: there is currently **no** `select!` or `timeout` around any transport call. Phase 4's fallback timeouts must go around *connection establishment*, never around an in-flight `send_chunk`/`receive`.
- **Documentation debt.** `docs/plans/peer-to-peer-transport-plan-2026-08-20.md` says the framing goes into `docs/protocol.md` only when the QUIC transport lands, and `docs/README.md` forbids writing up planned behaviour as shipped. The ALPN string and the sender-opens-the-stream rule belong in `protocol.md` at the same time.