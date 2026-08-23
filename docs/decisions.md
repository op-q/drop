# Decisions

Choices that are expensive or confusing to reverse, and the reasoning behind
them. Add an entry when changing the persistence stance, the session lifecycle,
the encryption model, the deployment shape, or the resource bounds.

Each entry records what was decided and what it costs. A decision that turns out
to be wrong is superseded by a new entry rather than edited into agreement with
the present.

## 1. The relay never persists file bytes

**Decision.** File bytes exist only in memory while being relayed between two
connected peers. The server writes no application storage.

**Why.** It is the product. An ephemeral relay has a much smaller surface than a
service that stores files: no deletion policy, no retention question, no backup
containing user data, no storage bill.

**Consequences.** Both peers must be online simultaneously. Resume after a
disconnect cannot be solved server-side. Adding persistence requires an explicit
product and threat-model decision, not an incremental commit.

The CLI may write a local temporary file to compress a payload, and must delete
it on every exit path including interruption. That is a client-side exception,
not a relaxation of the server rule.

## 2. The exact payload length is declared before the first byte

**Decision.** A sender commits to a total byte count at session creation, and
`meta` must match it.

**Why.** It lets the relay enforce the size limit before buffering anything,
gives both ends real progress and ETA, and lets the receiver detect a truncated
transfer instead of writing a short file and calling it done.

**Consequences.** Compression cannot be streamed. A compressed payload is
written to a temporary file first to learn its length, which costs local disk
and a second pass — worth it for source trees and documents, wasted on media
that is already compressed. This is why compression is off by default.

A file that changes size while being read is padded or truncated to the length
recorded at scan time, with a warning, because the declared total is already
committed.

This constraint survives encryption: an AEAD adds a fixed tag per chunk, so
ciphertext length stays a deterministic function of plaintext length.

## 3. Buffered bytes are bounded server-wide, not per session

**Decision.** One 200 MiB ceiling covers relayed file data across all sessions.
Each buffered chunk holds a reservation against it.

**Why.** A per-session bound multiplies out to sessions × capacity × chunk size,
which cannot grow with the chunk size and stay inside a container memory limit.
A shared budget decouples the two: one transfer may use a large window, while a
hundred concurrent transfers share the ceiling instead of multiplying it.

**Consequences.** Throughput on a busy relay depends on total load, not only on
one transfer's window. A reservation is returned when its chunk reaches the
receiver and when a session is discarded, so an abandoned transfer cannot
strand capacity.

## 4. One process, one replica

**Decision.** Sessions and live transfer channels exist only in process memory.
The Kubernetes Deployment runs exactly one pod with a `Recreate` strategy.

**Why.** Session state is a live pair of WebSockets and bounded channels, not a
row. Two replicas would split a sender and receiver across processes that cannot
see each other. `Recreate` avoids that during a rollout.

**Consequences.** No horizontal scaling. A rollout interrupts active transfers,
which is why the server drains on `SIGTERM` and reports readiness separately
from liveness. Scaling out later needs shared session coordination and
transfer-aware routing; session affinity alone cannot recover a live WebSocket.

## 5. One sender and one receiver per session

**Decision.** A code admits exactly one of each. Extra connections are refused.

**Why.** It makes the code a single-use capability rather than a channel anyone
can watch, and it makes a stolen code detectable: the legitimate receiver is
refused rather than silently sharing the stream.

**Consequences.** No multi-receiver fan-out. Rejoining after a dropped
connection is not possible, because the slot is consumed.

## 6. A folder is sent as one tar archive

**Decision.** The CLI archives a directory into a single tar stream rather than
transferring files individually.

**Why.** One declared length, one progress bar, one acknowledgement window. Per
file transfers would need their own framing and would make the declared-length
decision above much harder.

**Consequences.** The receiver must treat the archive as hostile input, which is
where the extraction rules in [`security.md`](security.md) come from. Sockets,
FIFOs, and device nodes are skipped. The whole transfer fails as a unit.

## 7. End-to-end encryption, with the key derived from the code by a PAKE

**Decision.** Payloads are encrypted client-side with AES-256-GCM. The key is
never transmitted and is never derived from anything the relay holds: both
peers run a SPAKE2 password-authenticated key exchange seeded by the secret
half of the transfer code, and derive the session key from its output via HKDF.
A relay, or anyone who compromises one, sees ciphertext and a byte count.

**The code is split, and this is load-bearing.** A code reads
`7F2A91-crossover-clockwork-ridge`. The leading nameplate is allocated by the
relay and is the only part sent to it; the three words are the PAKE password
and never leave either client.

The first draft of this entry made the whole code the password and also handed
it to the relay for routing. That is broken, and was caught during
implementation on 2026-08-21: SPAKE2 protects a transfer only while the
attacker does not know the password, and the relay is an attacker here. A relay
given the password can run the exchange against both peers at once and sit in
the middle reading and rewriting everything. The same flaw would have applied
to entry 10, where a DHT record keyed on a secret would let anyone grind the
word space offline and recover it.

So the routing half and the authenticating half have to be different bytes.
Only the routing half is ever published, and it carries nothing.

**Why AES-256-GCM rather than XChaCha20-Poly1305.** The earlier draft leaned
XChaCha for its 192-bit nonce, which is forgiving of random nonce generation.
That hedge buys nothing here: the key is freshly derived per transfer and never
reused, so a counter-based 96-bit nonce cannot collide within the only lifetime
that matters. Against that, AES-GCM is native in WebCrypto — no bundled cipher
in the browser client — and hardware-accelerated through AES-NI at both ends.

**Why a PAKE rather than appending the key to the code.** A 256-bit key beside
the code makes the shareable string roughly 52 base32 characters. That is
pasteable but not speakable, and a transfer code that cannot be read aloud over
a phone loses a real use case. SPAKE2 keeps the code short because an attacker
gets exactly one guess: a wrong password yields a key that fails the first
authenticated frame, and the session is burned. Guessing is online-only and
non-repeatable, so code entropy no longer has to carry the security of the
payload.

Three words, 33 bits. That is not a lot, and it does not need to be: the
comparison is against a single online try, not an offline cracking rate. What
enforces the single try is that the relay consumes a session when the first
receiver claims it, so a wrong guess costs the attacker the session rather than
letting them iterate.

**Consequences.** A handshake round trip is added before the first byte moves.
The browser client needs a SPAKE2 implementation, which WebCrypto does not
provide — a WASM or JavaScript dependency, and the one place this decision
costs the web client more than the CLI. A failed handshake must consume the
session rather than allow a retry, or the one-guess property is lost.

The envelope is deliberately transport-independent: the same encrypted chunk
format is carried over the peer-to-peer transport in entry 10 and over the
WebSocket relay. That is what lets the relay become an untrusted fallback
rather than a component to be removed.

**The claim this authorises.** AGENTS.md previously forbade describing Drop as
end-to-end encrypted. That rule is replaced, not deleted, by a narrower one:
CLI-to-CLI transfers are end-to-end encrypted; browser transfers are encrypted
in the browser but are only as strong as the JavaScript the site delivered,
which defeats a passive operator and stored traffic but not a server that
actively serves modified client code. The two cases must never be described in
wording that blurs them.

## 8. The hosted instance is a split deployment

**Decision.** `drop.lifbom.com` serves the browser client and `install.sh` as
static files; the relay answers on `api.drop.lifbom.com`. Both the frontend
build and the CLI's compiled default point at the API origin.

**Why.** The two halves already deploy independently, and the README documents
that split as a supported shape. Naming the API host separately also keeps both
clients off a provider-generated hostname: if the relay is recreated or moved,
DNS changes and nothing has to be rebuilt or re-released.

**Consequences.** The CLI's default and the URL a person types into a browser
are deliberately different hosts, which looks like a mistake unless it is
written down — this entry is that record. `DEFAULT_SERVER` in
[`client.rs`](../cli/src/client.rs) is compiled in, so an installed binary
cannot be redirected without `--server`, `DROP_SERVER`, or a new release; the
API hostname therefore has to stay stable once binaries ship.

The failure mode when it is wrong is at least loud and safe: pointing the CLI
at the static host returns 404 from `POST /api/session/create` rather than
silently transferring somewhere unexpected.

## 9. A colliding single file is numbered; a colliding archive entry is skipped

**Decision.** When a received file's name is already taken, the receiver saves
it beside the original with a number added — `report.pdf` becomes
`report-1.pdf`. When an entry inside a tar collides, that entry is skipped and
reported while the rest of the extraction continues. Neither replaces what the
receiver already has unless `--force` was given.

**Why.** Refusing was the old behaviour for a single file, and it failed both
peers: the receiver gave up before a byte moved, dropped the socket, and the
relay could only tell the sender its peer had disconnected. The session was
consumed either way, so a filename collision cost both people the whole
transfer and told them almost nothing about why.

Protecting the receiver's existing files never required refusing the transfer.
Numbering upholds the invariant and completes the transfer.

**Why the two halves differ.** The asymmetry is deliberate and reads as an
inconsistency, which is why it is recorded here. A single file is one object
the receiver asked for; giving it a free name delivers exactly what was sent. An
archive is a tree the sender laid out, and renaming individual files inside it
produces a directory matching neither what was sent nor what was already there
— a half-merged tree is harder to reason about than a reported skip.

**Consequences.** A receiver that repeatedly accepts the same file accumulates
numbered copies rather than being told to intervene, so the disk fills quietly.
The name is claimed with `create_new` rather than an `exists` check followed by
a create, so two receivers running side by side cannot settle on the same path.

## 10. Peer-to-peer by default, with the relay kept as an untrusted fallback

**Decision.** The CLI connects the two peers directly over QUIC using `iroh`,
with NAT hole-punching and n0's public relay infrastructure for the cases where
a direct path cannot be established. Peers find each other from the short code
alone: an ed25519 keypair is derived from the code, the sender publishes its
signed node address to the mainline DHT under that key via `pkarr`, and the
receiver resolves it. No Drop-operated server is involved in a peer-to-peer
transfer.

**Amended 2026-08-23: the keypair is derived from the nameplate, not the whole
code.** Entry 7 splits the code into a public nameplate and secret words, and a
DHT record keyed on the secret half would let anyone grind the word space
offline against a public record. Only the nameplate may be published. The
consequences paragraph below still describes the pre-split design where the
whole code is the DHT key; the disclosure it names is real either way, but it
is enumeration of the nameplate that causes it.

The existing WebSocket relay is not removed. It becomes the fallback path for
the cases the peer-to-peer path cannot serve, and it is no longer trusted with
plaintext because of entry 7.

**Why.** Relaying every byte is the project's only unavoidable running cost and
its only unavoidable hosting dependency. Hole-punching removes both for the
common case. It also removes the relay's bandwidth ceiling as a limit on
transfer speed, and the single-replica constraint in entry 4 stops applying to
peer-to-peer transfers because there is no session in any Drop process.

**Why the relay survives anyway.** Three cases need it, and none of them are
rare enough to drop:

- browsers cannot speak QUIC to an arbitrary peer, so the web client keeps the
  WebSocket path;
- networks that block UDP or the mainline DHT — a common corporate shape —
  cannot do rendezvous or hole-punching;
- a peer behind a symmetric NAT on both ends may never establish a direct path.

**Consequences.** Two transports must be kept interoperable at the envelope
level, which is the cost entry 7's transport-independent chunk format is paying
for. The dependency footprint of the CLI grows substantially — QUIC, DHT, and
PAKE are all new — against a binary that ships prebuilt for four targets.

Publishing to a public DHT under a key derived from a low-entropy code means an
attacker who enumerates codes can learn a sender's IP address and node
identity, even though the PAKE stops them reading any bytes. That is a new
disclosure with no counterpart in the relay-only design, and it is the reason
the code needs materially more entropy than the six hexadecimal characters used
today. It must be recorded in `security.md` rather than discovered later.

Peer discovery through third-party infrastructure — the mainline DHT and n0's
relays — replaces a dependency on the operator's own server with a dependency
on public networks the project does not control. Availability of a transfer now
depends on infrastructure nobody involved is paying for.

## 11. The browser runs the envelope as WebAssembly, not a second implementation

**Decision.** The envelope is its own crate, `crypto/`, and `crypto-wasm/`
compiles it to WebAssembly for the browser. The web client implements no
cryptography of its own: the SPAKE2 transcript, the chunk framing, the HKDF
info strings, the nonce layout, and the 2048-word list all come from the same
Rust the CLI runs.

**Why not TypeScript.** WebCrypto covers AES-256-GCM and HKDF — that is why
entry 7 chose AES-GCM over XChaCha20. It does not cover SPAKE2, and no browser
primitive does. So the choice was between writing a PAKE by hand over an
elliptic-curve library and compiling the one that already exists. Writing one
would have created a standing obligation: every item listed above would have to
stay byte-identical across two languages, indefinitely, and a mismatch would
surface as transfers failing in the field rather than as a build error. Entry 7
names overclaiming as the largest risk in this work, and two implementations
that almost agree is the same class of problem.

**What it costs.** The web build now needs a Rust toolchain and the
`wasm32-unknown-unknown` target, so it is no longer a pure Node build. The
module is 251 KB, 108 KB compressed, against roughly 40 KB for a hand-written
equivalent. That is small beside the payload of a file transfer and is paid
once per visit rather than once per transfer.

**What it does not buy.** It does not make a browser transfer as strong as a
CLI one. The page fetches this module from the same origin that serves the
JavaScript, so a server willing to serve modified client code will serve a
modified envelope just as easily. The limit entry 7 draws sits exactly where it
did; this decision is about correctness and maintenance, not about trust.

**Consequences.** `cli/src/crypto/` became the `drop-crypto` crate, re-exported
by the CLI under the same name so its call sites read unchanged. Two
generations of `getrandom` reach the wasm target by different dependency paths
and each needs its backend named explicitly; missing one fails the build rather
than yielding a module with no entropy, and `web/tests/envelope.test.mjs` pins
that property by asserting two handshakes differ. Interoperation in both
directions is held by `web/tests/interop.test.mjs`, which runs a real relay and
the real CLI.

