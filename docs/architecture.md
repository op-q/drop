# Architecture

What the pieces are, what runs where, and which direction the project is
moving. This is the orientation document: read it before the others when the
question is "where does this code live and why".

It owns no contract. The wire format belongs to [`protocol.md`](protocol.md),
the trust boundaries to [`security.md`](security.md), and the reasoning behind
costly choices to [`decisions.md`](decisions.md).

## What Drop is trying to be

**A file transfer between two terminals that needs no service in the middle,
and that nobody in the middle could read even if there were one.**

Those are two separate goals and they landed in that order deliberately:

1. **Nobody in the middle can read it.** Done. The payload is sealed by the
   sender and opened by the receiver under a key derived from the transfer
   code; the relay carries an envelope it has no way into. See
   [`decisions.md`](decisions.md) entry 7.
2. **There is nobody in the middle.** In progress. Two CLIs connect directly
   over QUIC and find each other through a record derived from the public half
   of the code, so no Drop-operated server takes part. See
   [`decisions.md`](decisions.md) entry 10 and
   [`plans/peer-to-peer-transport-plan-2026-08-20.md`](plans/peer-to-peer-transport-plan-2026-08-20.md).

The relay is not being removed. It stays as the fallback for browsers, for
UDP-blocked networks, and for the NAT cases hole-punching cannot solve. What
changes is that it stops being *required*, and encryption is what makes falling
back to it acceptable.

Order matters here: the envelope had to land first, because an untrusted relay
is what makes a fallback to the relay defensible. Building the direct path
first would have produced a fast path with no honest story for the slow one.

## The crates

The repository is one Cargo workspace with four members plus a web client that
is not a Cargo crate.

| Path | Crate | What it is |
| --- | --- | --- |
| `src/` | `api` | The **relay server**. Axum, WebSockets, an in-memory session map. This is what gets deployed. |
| `cli/` | `drop-cli` | The **`drop` binary**. What a user installs; sends and receives. |
| `crypto/` | `drop-crypto` | The **envelope**: codes, SPAKE2, HKDF, AES-256-GCM chunk framing. No I/O, no transport. |
| `crypto-wasm/` | `drop-crypto-wasm` | `crypto/` compiled to WebAssembly for the browser. Bindings only. |
| `web/` | — | Svelte and TypeScript browser client. Loads the WebAssembly above. |

### What depends on what

```text
        drop-crypto  ◀── the only thing both halves share
        ╱         ╲
  drop-cli         drop-crypto-wasm ◀── web/
  (the `drop`             (browser)
   binary)

  api ◀── deployed alone; depends on drop-crypto for version and limit constants
```

**The CLI does not depend on the relay.** `cli/Cargo.toml` lists
`api = { path = ".." }` under `[dev-dependencies]`, not `[dependencies]`, so it
is compiled for `cargo test` and never into the shipped binary. That entry
exists for two reasons:

- `cli/tests/transfer.rs` spawns a **real relay in-process** rather than mocking
  one, so an end-to-end test exercises the actual server code.
- `cli/tests/protocol.rs` asserts the two crates still agree on shared
  constants — `api::config::ENVELOPE_VERSION` against
  `drop_cli::crypto::ENVELOPE_VERSION`, and the sealed metadata size against
  `MAX_OPAQUE_FIELD_BYTES`. A drift that would fail transfers in the field
  fails the build instead.

The consequence worth holding onto: **an installed `drop` binary contains no
relay code at all.** Once the direct transport lands, a CLI-to-CLI transfer runs
nothing from `src/`, not even transitively. That separation is what makes the
goal above reachable rather than aspirational.

## Where a transfer's work happens

| Machine | Runs | Holds |
| --- | --- | --- |
| Sender | `drop send` | the file, the words, the derived keys |
| Receiver | `drop recv` | the derived keys, the written file |
| Relay | `api` | a nameplate, a byte count, an opaque blob in flight |
| Browser | `web/` + wasm | the same envelope as the CLI, delivered by a server |

The relay's row is the short one on purpose. It holds no filename, no key, and
no bytes it can read; `Session` has no filename field to hold one.

## The path of a transfer today

```text
sender                          relay                        receiver
  │ POST /api/session/create ──▶ │ allocates a nameplate
  │ ◀── nameplate ───────────────│
  │ draws 3 secret words locally
  │ ─────────── the human carries nameplate-word-word-word ──────────▶ │
  │ WS /ws/upload/{nameplate} ─▶ │ ◀─ WS /ws/download/{nameplate} ──── │
  │                              │ pairs exactly one of each
  │ ◀── key_exchange ──────────▶ │ ◀── key_exchange ─────────────────▶ │
  │ sealed meta ───────────────▶ │ ──────────────────────────────────▶ │
  │ sealed chunks ─────────────▶ │ ──────────────────────────────────▶ │
  │ ◀── acknowledgements ────────│ ◀─────────────────────────────────  │
```

The relay is a `HashMap<nameplate, Session>` holding two bounded channels. It
forwards, meters, and bounds. It does not translate the payload and cannot.

It *does* translate some control frames, which is a wrinkle rather than a
design: see [`protocol.md`](protocol.md) on the peer vocabulary, and
[`decisions.md`](decisions.md) entry 12.

## The transport seam

`cli/src/transport/` is where the CLI stops caring what carries a transfer.

```text
cli/src/transport/
├── mod.rs        Transport trait, Frame, TransportError
├── relay.rs      RelayTransport — the WebSocket
└── scripted.rs   ScriptedTransport — replays a fixed conversation, tests only
```

The trait is the whole conversation and nothing else:

```rust
async fn await_peer(&mut self) -> Result<(), TransportError>;   // default: ready
async fn send_control(&mut self, frame: Value) -> Result<(), TransportError>;
async fn send_chunk(&mut self, chunk: Vec<u8>) -> Result<(), TransportError>;
async fn receive(&mut self) -> Result<Option<Frame>, TransportError>;
async fn close(&mut self);
```

Two boundaries are deliberate and easy to get wrong later:

- **Establishing a connection is not on the trait.** A relay is reached at an
  origin URL with a nameplate it allocated over HTTP; a direct peer is reached
  by resolving a record and punching a hole. Those constructors share no
  arguments, so each transport module owns its own.
- **The envelope does not appear here.** Chunks arrive sealed and leave sealed.
  A transport that could tell the difference would be a transport that could
  read the payload.

`send::send_transfer` and `recv::receive_transfer` are generic over
`T: Transport`, so a second carrier is a different `T` rather than a second copy
of the transfer loop.

## Where this is going

Tracked in [`implementation-checklist.md`](implementation-checklist.md); the
reasoning is in the plans. In short:

- **QUIC transport** — a second implementation of the trait over `iroh`, one
  bidirectional stream per transfer. QUIC gives an ordered byte stream with no
  message boundaries, so this transport carries its own framing; the WebSocket
  got that free from its text/binary opcode.
- **Rendezvous** — an ed25519 keypair derived by HKDF from the **nameplate**,
  under which the sender publishes its node address to the mainline DHT and the
  receiver resolves it. Never derived from the words: a record keyed on the
  secret half would let anyone grind 33 bits offline against a public record.
- **Selection and fallback** — try direct, fall back to the relay, and say which
  path was taken.

The known cost, recorded rather than discovered: a nameplate is small and
public, so enumerating it against the DHT discloses a sender's IP and node
identity. That is a new weakness with no counterpart in the relay design, and
the PAKE is what keeps it from being a disclosure of bytes.
