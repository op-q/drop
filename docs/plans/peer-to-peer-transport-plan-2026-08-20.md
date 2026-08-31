# Peer-to-peer transport plan

Status: **active**
Created: **2026-08-20**
Last updated: **2026-08-29**

## Goal

Two `drop` CLIs move a file between them with no Drop-operated server in the
path. The relay stops being required and becomes a fallback.

Decision recorded at [`../decisions.md`](../decisions.md) entry 10. This plan is
the implementation of that decision and depends on the envelope from
[`end-to-end-encryption-plan-2026-08-19.md`](end-to-end-encryption-plan-2026-08-19.md),
which must land first.

## The problem this has to solve

`iroh` connects to a `NodeId` — a 32-byte ed25519 public key, about 52 base32
characters. Handing that to a person defeats the point of a short code. So the
real work here is not the QUIC connection, it is **rendezvous**: turning four
spoken words into a routable peer.

The answer is to derive a keypair from the code's public half:

```text
nameplate ──HKDF──▶ ed25519 keypair ──▶ pkarr record on the mainline DHT
                                          └─ value: sender's iroh NodeAddr
```

The sender publishes, the receiver derives the same keypair and resolves. Both
sides compute the DHT location from the nameplate alone, so nothing has to be
exchanged out of band beyond the code itself.

**The key is derived from the nameplate, never from the words.** This plan was
written before the code was split, and said "from the code". That is unsafe:
a DHT record keyed on the secret lets anyone grind the 33-bit word space
offline against a public record and recover the PAKE password, which is exactly
the failure [`../decisions.md`](../decisions.md) entry 7 records being caught on
2026-08-21. The nameplate is public, carries nothing, and is the only half that
may ever be published.

**This is why the PAKE is load-bearing.** The DHT record is public and the
nameplate is enumerable, so an attacker can resolve the record and attempt a
connection. SPAKE2 is what stops them getting bytes, and the one-guess property
is what stops them grinding. Without the PAKE this design would be
unshippable.

## Constraints and invariants

- The relay must remain fully functional. This plan adds a transport; it does
  not remove one.
- The envelope is identical on both transports. A CLI must be able to send to a
  browser over the relay and to a CLI over QUIC using the same code semantics.
- Fallback must be automatic and observable — a user should not have to know
  what a DHT is, but should be able to find out which path a transfer took.
- The relay's resource bounds do not apply to peer-to-peer transfers, because
  no Drop process is involved. The 4 GiB limit is a relay limit; decide
  deliberately whether it still applies peer-to-peer.

## Non-goals

- Removing the relay. Browsers, UDP-blocked networks, and doubly-symmetric NAT
  all still need it.
- Running Drop-operated relay or discovery infrastructure for the QUIC path.
  n0's public relays and the mainline DHT are used as-is.
- Identity. Knowing the code proves knowledge of the code, nothing more.

## Phases

### Phase 1 — Transport abstraction

Done 2026-08-24.

- [x] Define a transport trait the send and receive paths use: send a control
      frame, send a chunk, receive, close. **Establish is deliberately not on
      the trait** — see the findings below.
- [x] Move the existing WebSocket client behind it with no behaviour change.
      `cli/src/transport/relay.rs` holds the socket; `cli/src/client.rs` keeps
      only the relay's HTTP API, which is relay-specific by nature since a
      transfer that needs no server creates no session.
- [x] Gate: the full existing test suite passes against the refactored relay
      transport before any QUIC code exists. 113 tests, up from 104: the nine
      new ones drive the transfer paths over `ScriptedTransport`, a second
      implementation of the trait that is not a socket. Without a second
      implementation the trait is an assertion rather than a seam.

#### Findings

**Establish is not on the trait.** The relay is reached at an origin URL with a
nameplate it allocated over HTTP; a direct connection is reached by resolving a
record and punching a hole. Those constructors share no arguments, so each
transport module owns its own and the choice between them belongs at the single
call site in Phase 4 that makes it. Putting `establish` on the trait would have
produced a parameter bag every implementation ignores half of.

**The relay is not a dumb pipe for control frames — it translates, and Phase 2
has to decide who does that instead.** This is the substantive thing Phase 1
surfaced, and it is not mechanical:

| The receiver sends | The sender hears | Produced by |
| --- | --- | --- |
| `chunk_ack` | `ack` | the relay, renaming |
| `complete` | `status: transfer_complete` | the relay, after the receiver confirms its byte count |
| — | `status: receiver_connected` | the relay, on claim |
| — | `status: sending` | the relay |

`wait_for_receiver` and `await_completion` both block on frames no peer ever
sends. Over a direct connection there is no third party to invent them. The two
honest options:

1. **The QUIC transport synthesizes them.** `receiver_connected` when the
   connection is established, and the receiver's own `complete` mapped to
   `transfer_complete`. The transfer paths stay untouched and every transport
   presents the same vocabulary. The cost is that a transport starts having
   opinions about protocol semantics rather than only carrying frames.
2. **The paths learn the peer's vocabulary** and treat the relay's extra
   statuses as the relay's own embellishment. Cleaner in the long run, but it
   changes the relay path too, which Phase 1 was careful not to.

Option 1 is the smaller change and keeps the relay path frozen while QUIC is
unproven; option 2 is where this should end up. Decide at the start of Phase 2,
not during it, and record it in [`../decisions.md`](../decisions.md) if it goes
the second way, because that changes the wire contract.

**Error wording outlives the relay.** `the relay reported an error`, `the relay
sent more bytes than the transfer declared`, and `relay_error` are user-facing
strings on paths that will no longer always involve a relay. Left alone here on
purpose: Phase 1 promised no behaviour change and an error string is behaviour.
Fix with Phase 4, when a transfer can actually take either path and the word is
wrong rather than merely imprecise.

### Phase 2 — QUIC transport

- [x] **Settle the control vocabulary first.** Decided 2026-08-24 and recorded
      as [`../decisions.md`](../decisions.md) entry 12: the paths speak the
      peer's vocabulary and a relay only embellishes it. The sender accepts
      `chunk_ack` and the receiver's `complete` alongside the relay's `ack` and
      `status: transfer_complete`, and checks the byte count itself when it
      arrives directly. Waiting for the peer became `Transport::await_peer`,
      because whether a peer must be waited for is a property of the carrier
      rather than of the protocol. Nothing on the wire changed, so no released
      client is affected.

      The receive path needed no changes at all — the receiver already spoke
      peer vocabulary, and only the sender was listening for things the relay
      invented. That is the strongest evidence available that this was the
      right half of the seam to move.
- [x] **Map the control messages onto stream framing.** Done 2026-08-24 as
      `cli/src/transport/framed.rs`, ahead of the connection rather than after
      it, because the framing is the part that needs no network to test:

      ```text
      ┌──────┬────────────────┬─────────────────┐
      │ kind │ length (BE u32)│ payload         │
      │ 1 B  │ 4 B            │ `length` bytes  │
      └──────┴────────────────┴─────────────────┘
        0x01 control — UTF-8 JSON
        0x02 chunk   — sealed bytes, opaque to the transport
      ```

      Written against `AsyncRead`/`AsyncWrite` rather than against QUIC, so the
      QUIC transport becomes a thin wrapper and the framing itself is exercised
      over an in-memory pipe. The length is read before the payload, so it is
      an allocation request from an unauthenticated peer: it is checked against
      a ceiling of one sealed chunk before a byte is read.

      A stream that ends **between** frames is the peer finishing; one that
      ends **inside** a header is a truncation. `read_exact` reports both as
      `UnexpectedEof`, so the header read counts bytes itself — reporting a
      truncation as a clean end would turn a cut-off transfer into a
      successful-looking short read.
- [x] **A whole transfer over a bare byte pipe**, sender to receiver, with no
      relay and no socket in the path —
      `a_whole_transfer_crosses_a_bare_byte_pipe`. This is the first transfer
      in the project involving no Drop-operated process at all, and it also
      pins entry 12: over a pipe the sender is ready immediately, hears the
      receiver's own `chunk_ack`, and finishes on the receiver's own `complete`
      after checking the count itself. If the sender still required the relay's
      wording, it would hang.
- [x] `iroh` (1.0.3) endpoint, one bidirectional stream per transfer. Done
      2026-08-25 as `cli/src/transport/quic.rs`, with
      `a_whole_transfer_crosses_a_quic_connection` carrying a 2 MiB file end to
      end over a real connection with no Drop server in it.
- [ ] Direct connection and n0-relay-assisted connection both exercised. **Only
      direct is.** Both test endpoints bind with `RelayMode::Disabled` and meet
      over loopback, so nothing here has exercised a relay, a home relay, or
      hole punching. That is the same gap section 6 of the API survey names, and
      it does not close on this machine.

Two things the connection settled that the framing work could only assume:

- **The sender accepts the connection but opens the stream**, and the receiver
  dials but accepts it. `accept_bi` resolves when the peer first *writes*, not
  when it opens a stream, and Drop's sender is the one that speaks first.
  Reversed, both sides park forever instead of failing.
- **Only the sender may close.** A `CONNECTION_CLOSE` permits the peer to drop
  stream data it received but has not yet handed up, acknowledged or not. The
  receiver's last act is writing the `complete` that the sender is blocked
  reading, so a receiver that closed immediately destroyed it in flight and the
  sender reported `connection lost` for a transfer whose file was already
  correct on disk. iroh states the rule on `Connection::close`: only the peer
  last *receiving* application data can be certain everything arrived. Here that
  is the sender, so `close()` branches on role and the receiver waits.

The framing now belongs in [`../protocol.md`](../protocol.md), since it ships.

### Phase 3 — Rendezvous

- [x] Derive an ed25519 seed from the **nameplate** by HKDF, domain-separated
      from every key in the encryption plan. Never from the words: see above.
      Done 2026-08-24 as `crypto/src/rendezvous.rs`, with
      `the_meeting_point_ignores_the_words` holding that property open. The
      derivation takes a whole `TransferCode` rather than a string so the
      nameplate is normalised — a receiver retyping it in lowercase has to
      arrive where the sender published.
- [x] Sender publishes its address as a `pkarr` record; receiver resolves. Done
      2026-08-25 as `cli/src/transport/rendezvous.rs`. The DHT is behind a
      `Directory` trait so the record layer — filtering, ticketing, signing,
      parsing — is tested against an in-memory implementation; see "What cannot
      be verified on the development machine" for why that split exists.
      `MainlineDirectory` itself is **untested** and says so in its own doc
      comment. The address is carried as an `EndpointTicket` in a `_drop` TXT
      record, and `record_for` filters through `publishable` so a private
      address cannot reach a record by a caller forgetting a step.
- [ ] Handle the publish/resolve latency honestly in the UI — this is seconds,
      not milliseconds, and the sender must not print "waiting" before the
      record is actually retrievable. The ordering the survey's measurements
      imply: bind, `online()`, publish, *then* print the code. Nothing calls any
      of this yet, so the ordering is not yet enforced anywhere.
- [ ] Republish while waiting, since DHT records expire.

### Phase 4 — Selection and fallback

- [x] Try peer-to-peer, fall back to the relay on rendezvous or setup failure.
- [x] `--transport p2p|relay|auto` to force a path, defaulting to `auto`.
      Also `DROP_TRANSPORT`.
- [x] Report the path taken, so a slow transfer is diagnosable.
- [x] The fallback must not leak: a transfer that falls back is still encrypted
      under the same envelope, and the relay still cannot read it.
- [x] A locally drawn nameplate, since a serverless send has nobody to allocate
      one. `TransferCode::generate` draws one and resolves it first, which both
      catches a collision and proves the DHT answers before a code is shown.

#### Where the fallback can and cannot happen

Established 2026-08-29, and it is narrower than the bullet above reads.

**A failed hole punch is not a failed connection.** iroh does hole punching
"complemented by relay servers under the hood" — when a direct path cannot be
established it carries the same QUIC connection over n0's relay instead, and
neither peer has to do anything. So the Drop relay is not a fallback for
connectivity. It is a fallback for **rendezvous and setup**: binding, reaching a
home relay, publishing to the DHT, resolving from it.

That places every fallback decision *before the code is printed*, which is what
makes the design tractable:

```text
sender                                     receiver
  bind, online, publish ──┐                  resolve the nameplate ──┐
    ok  → print a code    │                    found → dial          │
    fail→ relay session ──┘                    miss  → relay ────────┘
          (a different nameplate)
```

The receiver can decide per transfer because the nameplate tells it where to
look: a record under that nameplate means the sender went direct, and its
absence means the sender fell back. **Once the sender has printed a code the
path is fixed**, because a relay-allocated nameplate and a locally drawn one are
different strings and the code names one of them.

**What this does not cover**, and it must be documented rather than discovered:
a sender that published, and a receiver that resolved but cannot reach it at all
— both behind symmetric NAT *and* unable to reach n0's relay. The receiver falls
back to the Drop relay and finds nobody, because the sender is not there. Rare,
and the honest fix is a sender that holds both paths open at once, which needs
the relay to accept a nameplate it did not allocate.

### Phase 5 — Documentation

- [ ] README: what runs where, and that a peer-to-peer transfer needs no
      server.
- [ ] `security.md`: the DHT address-disclosure weakness from entry 10, stated
      plainly.
- [ ] `protocol.md`: the QUIC framing, so a third client can interoperate.

## Who enforces one guess when there is no relay — settled

Found 2026-08-24 while deriving the rendezvous key. **Settled 2026-08-25 and
recorded as [`../decisions.md`](../decisions.md) entry 13: the sender enforces
it, and asks a human before granting another attempt.** The two details the
proposal left open are decided with it, at the bottom of this section. The
problem statement below is kept because the reasoning is worth not losing.

The whole security argument for a 33-bit password is that an attacker gets
**one** attempt. The plan above already says so. What it does not say is what
enforces it, and the answer today is the relay: `claim_receiver` refuses a
second claim on a session, so a wrong guess consumes the session and the real
receiver is turned away. That is a server-side mechanism, and a serverless
transfer does not have it.

Without it the attack is straightforward and cheap:

1. Enumerate a nameplate — 24 bits, and the DHT answers.
2. Derive the same rendezvous keypair; the input is public, so this is free.
3. Connect to the sender and run SPAKE2 with a guessed password.
4. SPAKE2 reveals nothing on failure, but the sealed metadata does: it either
   opens or it does not. That is one bit per attempt.
5. Disconnect and repeat.

An online oracle with no limit is not 33 bits of security, it is 33 bits of
work at network speed. Nothing in the envelope prevents this, because the
envelope was never what was providing the guarantee.

**The proposed answer is that the sender enforces it.** The first peer to
complete a key exchange is the transfer's one counterpart. If that peer fails
to produce a valid acknowledgement, or disconnects mid-handshake, the sender
stops rather than waiting for another connection. That reproduces exactly what
the relay's single claim provided, on the only participant that a direct
transfer is guaranteed to have.

It carries the same cost the relay design already carries and already
documents: an attacker who guesses a nameplate can burn a transfer without
learning anything, which is denial of service. `security.md` says that of the
relay path today, and it stays true here.

Two details needed deciding with it. Both are now decided:

- **Does a genuine mistype cost the sender the transfer?** No, but only with a
  human's consent. The sender prints what happened and asks whether to allow
  another attempt; declining, or running with no terminal, ends the transfer.
  This beats both a flat one-attempt rule and a bounded retry count, because an
  attacker grinding the code then needs human approval per guess — capping the
  attack at human speed and, more importantly, making it *visible* to the person
  being attacked.
- **When does the sender consider a peer to have committed?** On the peer's
  response to the sealed metadata, not on the handshake. Completing SPAKE2
  proves nothing — an attacker completes it trivially, which is the whole reason
  a wrong password fails at the metadata instead. An explicit failure, a timeout
  and a disconnect all count as one consumed attempt, which makes the honest
  mistyper and the silent attacker indistinguishable to the sender, as they
  should be.

The QUIC path still must not be reachable by default until this is
*implemented*. Deciding it does not enforce it.

### Building it

Broken in two, because the halves fail differently and only the first touches
the wire.

**A — the checkpoint after `meta`.** The sender streams the whole payload today
without ever learning whether the peer opened the metadata, so there is no
moment at which a failed guess is observable. Add one:

- [x] `Transport::peers_enforce_one_guess()`. The relay answers no — it refuses
      a second claim on a session, so a wrong guess is burned server-side. A
      direct connection answers yes. No default implementation: a carrier that
      forgets to state this should fail to compile, because both wrong answers
      are security bugs rather than papercuts.
- [x] Receiver: after `open_metadata` succeeds send `meta_ok`, after it fails
      send `error` and stop — **only when the carrier says the peers enforce
      it.** This cannot be additive, and that is the trap: the relay parses
      receiver frames into a typed enum and calls `fail_session` on anything it
      does not recognise (`src/routes/download_ws.rs:322`), so a receiver that
      sends `meta_ok` unconditionally breaks every relay transfer against the
      deployed relay.
- [x] Sender: wait for `meta_ok` before the first chunk, bounded by
      `META_CHECKPOINT_TIMEOUT`. An explicit `error`, a timeout, a disconnect
      and any other frame all resolve to one consumed attempt, per entry 13.
- [x] `send_transfer` reports that outcome distinctly rather than as a generic
      error, since the caller has to tell "the peer failed the code" apart from
      "the network broke" to know whether prompting is even the right response.
- [x] Gate: a wrong code over a direct connection stops the sender before a
      single chunk is written, and the relay path's frames stay byte-identical
      to today's. Both are testable with no network — `ScriptedTransport` for
      the sender's policy, an in-memory duplex for the pair.

**B — the attempt loop and the prompt.** `QuicEndpoint::accept_transfer`
consumes the endpoint, so a failed attempt currently ends the process: strict
one-attempt, which is the behaviour entry 13 rejected because a mistype should
not cost the transfer.

- [x] Accept without consuming the endpoint, so a second attempt has somewhere
      to land. `iroh::Endpoint` is `Clone`, and `QuicTransport::close` has to
      stop closing an endpoint it no longer solely owns.
- [x] Prompt on stderr and read from the TTY, in entry 13's wording. Count the
      attempts and show the count: a climbing counter is the whole point of
      choosing the prompt over a retry limit.
- [x] No TTY means strict — end the transfer. That is the safe direction.
- [x] Gate: two failed guesses need two separate approvals, and a
      non-interactive sender ends after the first.

Deliberately out of scope here: which transport `drop send` picks. That is
Phase 4. This makes the direct path *safe* to reach, not reachable.

#### Findings

Done 2026-08-29. 153 tests, up from 145.

**The carrier split had to be explicit, and nearly was not.** The first shape
tried was a receiver that always sends `meta_ok` and a sender that ignores it
over the relay — additive, no trait change, no relay change. It would have
broken every relay transfer in production. `src/routes/download_ws.rs:322`
parses receiver frames into a typed `ReceiverMessage` and calls `fail_session`
on a parse error, so an unknown frame is not ignored, it is fatal. Hence a
trait method with no default rather than a convention.

**The payload comes back from a failed guess, and the type says so.** The
checkpoint fires before `into_chunks`, so nothing has been read, compressed or
spooled. Returning `Attempt::FailedTheCode { payload, .. }` rather than an
error keeps a retry costing a connection instead of a recompressed folder — and
it removed the downcast the first draft needed to tell a failed guess from a
broken network.

**A test can deadlock where the protocol cannot.** Restructuring the QUIC
ordering test to join both tasks before writing anything hung: `accept_bi`
resolves on the peer's first *write*, so the write has to stay inside the
sending task. The same ordering the module's own doc comment warns about, met
from the other direction.

**`AskTheTerminal` takes its TTY answer at construction.** Reading
`stdin().is_terminal()` inside the prompt would make the unattended test pass or
block depending on how `cargo test` was launched.

## Risks

- **DHT address disclosure.** The nameplate is six hex characters, so
  enumerating it is cheap and reveals sender IP and node identity to anyone,
  without revealing bytes. New weakness with no counterpart in the relay
  design. Must be documented, not discovered — and it is the reason the
  nameplate's entropy is an open question below rather than a settled detail.
- **Third-party infrastructure.** Transfers now depend on the mainline DHT and
  n0's relays — public networks nobody involved is paying for or operating. An
  outage there is an outage here, and there is no support relationship.
- **Dependency weight.** QUIC, DHT, and PAKE against a binary that ships
  prebuilt for four targets. Watch the size and the cross-compilation story;
  the `flate2` note in [`cli/Cargo.toml`](../../cli/Cargo.toml) exists because
  a C dependency already caused this pain once.
- **Hole-punching is not universal.** Symmetric NAT at both ends never
  connects directly. The fallback is not an edge case, it is a supported path.
- **Two transports is twice the protocol surface**, and the failure modes
  differ. The abstraction in Phase 1 is what keeps this from doubling the
  testing burden, so it is worth doing properly before Phase 2 rather than
  retrofitting.
- **The 4 GiB limit becomes ambiguous.** It exists to protect the relay. Peer
  to peer there is nothing to protect, but silently changing a documented limit
  based on invisible transport selection is worse than keeping it.

## What cannot be verified on the development machine

**Superseded 2026-08-29 — outbound UDP now works here, and the whole path has
been run end to end.** The section is kept because it explains the shape of
everything built before that date, and because the restriction it describes is
a property of an environment rather than of the code: it can come back.

### What the first real-network run proved, and what it cost

With `DROP_SERVER` pointed at a dead port and `--transport p2p` forbidding any
fallback, two CLIs moved 3,000,000 bytes byte-identical. Publishing to the
mainline DHT, resolving the same record from a second process, and dialling a
peer that had never been heard of all work. **That is the plan's gate, met.**

It failed on the first attempt, and the failure is the point. `dial_sender`
bound a `QuicEndpoint`, dialled, returned the `QuicTransport`, and let the
endpoint drop on the way out — so the connection died the instant the transfer
began, and the sender reported `connection lost` immediately after `Receiver
connected.`

The cause was a consequence of the one-guess work: `QuicTransport` stopped
owning its endpoint so the *sender's* endpoint could outlive a failed guess and
accept another attempt. That made endpoint lifetime the caller's job, the
sender's path did it right, and the receiver's did not. **Every loopback test
passed throughout**, because each holds both endpoints in the test function's
own scope for the whole test and so cannot express the bug. It is now pinned by
`dropping_an_endpoint_ends_the_transfer_on_it`, which asserts the consequence
rather than the fix.

The lesson worth keeping: the tests were not weak, they were *shaped* so that
one class of error was invisible. A local pipe and a loopback socket keep
objects alive that a real deployment does not.

### The original restriction

Recorded 2026-08-25, because it silently shapes what "the tests pass" is worth
for everything built before that date.

**Outbound UDP was blocked in the sandbox the agent tooling ran commands in.** A
`sendto` to any non-local address failed outright, and the system resolver hung
because it could not reach a nameserver. TCP was unaffected, so crates.io and
the toolchain downloaded fine and the restriction was easy to miss.

Everything the peer-to-peer path is *for* travels over UDP:

| | Needs | Was verifiable | Is now |
| --- | --- | --- | --- |
| Framing over a byte pipe | nothing | yes | yes |
| QUIC over loopback | local UDP | yes | yes |
| n0-relay-assisted connection | outbound UDP | **no** | yes |
| `pkarr` publish and resolve | outbound UDP (mainline DHT) | **no** | **yes, run** |
| NAT traversal and hole punching | outbound UDP | **no** | **still not** |

The last row has not moved and is worth being precise about. Both peers in the
run were on one machine behind one NAT, so whether a *direct* path was punched
or the connection rode n0's relay is not something that run distinguishes —
iroh does the same thing either way and the transfer succeeds regardless. Two
peers on genuinely different networks remains untested, and that is the row
that carries the feature's performance claim rather than its correctness.

So the QUIC tests pass, and they prove less than they look like they prove. Two
endpoints on one host with relays disabled exercise the framing, the stream
orchestration and the close ordering — all of which were worth pinning, and one
of which was wrong — but they do not exercise the feature's premise.

The consequence for Phase 3 is that `pkarr` publish and resolve can be *written*
here but not *run* here. Two honest ways forward, and they should be chosen
between rather than blurred:

- Write it with the DHT behind a trait, test the record's construction and
  parsing against an in-memory implementation, and mark the network path
  untested until someone runs it on an unrestricted machine.
- Run it unsandboxed. This publishes a real record to a public DHT, which is an
  outward-facing action rather than a local test, and it should be a deliberate
  decision rather than something that happens inside a test run.

## Validation

- [ ] Two CLIs on the same LAN transfer with no Drop server reachable at all —
      the strongest possible demonstration, and it should be a recorded run.
- [ ] Two CLIs behind different NATs transfer directly.
- [ ] With UDP blocked, the transfer completes over the relay and says so.
- [ ] A forced `--transport relay` transfer is byte-identical in outcome.
- [ ] CLI-to-browser still works and is unaffected.
- [ ] Binary size and cold-start time recorded before and after.
- [ ] Full validation command set passes.

## Open questions

- How much entropy does the nameplate need? It is 24 bits today, which is fine
  for naming a relay session and cheap to enumerate once it also keys a public
  DHT record. Widening it is a change to what the relay allocates and to what a
  person types, so it belongs here rather than in the encryption plan.
- Does the 4 GiB limit apply peer-to-peer? Leaning yes for consistency, with
  the reasoning written down rather than left to the transport.
- Should the sender publish to the DHT before or after the receiver is known to
  be looking? Publishing early costs exposure time; publishing late costs
  latency.
- Is there a privacy-preserving rendezvous that avoids publishing an address
  under a guessable key at all? Worth an hour of research before Phase 3, since
  it is the one genuinely new weakness this plan introduces.
- Can the web client reach a QUIC peer at all via WebTransport, or is the
  relay permanently the browser's only path? Assume the latter until measured.
