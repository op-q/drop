# Peer-to-peer transport plan

Status: **proposed**
Created: **2026-08-20**
Last updated: **2026-08-20**

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

The answer is to derive a keypair from the code itself:

```text
code ──HKDF──▶ ed25519 keypair ──▶ pkarr record on the mainline DHT
                                     └─ value: sender's iroh NodeAddr
```

The sender publishes, the receiver derives the same keypair and resolves. Both
sides compute the DHT location from the code alone, so nothing has to be
exchanged out of band beyond the words themselves.

**This is why the PAKE is load-bearing.** The DHT record is public and the code
is low-entropy, so an attacker who enumerates codes can resolve the record and
attempt a connection. SPAKE2 is what stops them getting bytes, and the
one-guess property is what stops them grinding. Without the PAKE this design
would be unshippable.

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

- [ ] Define a transport trait the send and receive paths use: establish,
      send a control frame, send a chunk, receive, close.
- [ ] Move the existing WebSocket client behind it with no behaviour change.
- [ ] Gate: the full existing test suite passes against the refactored relay
      transport before any QUIC code exists.

### Phase 2 — QUIC transport

- [ ] `iroh` (1.0) endpoint, one bidirectional stream per transfer.
- [ ] Map the existing control messages onto the stream framing.
- [ ] Direct connection and n0-relay-assisted connection both exercised.

### Phase 3 — Rendezvous

- [ ] Derive an ed25519 keypair from the code by HKDF, domain-separated from
      every key in the encryption plan.
- [ ] Sender publishes its `NodeAddr` as a `pkarr` record; receiver resolves.
- [ ] Handle the publish/resolve latency honestly in the UI — this is seconds,
      not milliseconds, and the sender must not print "waiting" before the
      record is actually retrievable.
- [ ] Republish while waiting, since DHT records expire.

### Phase 4 — Selection and fallback

- [ ] Try peer-to-peer, fall back to the relay on rendezvous failure,
      hole-punch failure, or timeout.
- [ ] `--transport p2p|relay|auto` to force a path, defaulting to `auto`.
- [ ] Report the path taken, so a slow transfer is diagnosable.
- [ ] The fallback must not leak: a transfer that falls back is still encrypted
      under the same envelope, and the relay still cannot read it.

### Phase 5 — Documentation

- [ ] README: what runs where, and that a peer-to-peer transfer needs no
      server.
- [ ] `security.md`: the DHT address-disclosure weakness from entry 10, stated
      plainly.
- [ ] `protocol.md`: the QUIC framing, so a third client can interoperate.

## Risks

- **DHT address disclosure.** Enumerating codes reveals sender IP and node
  identity to anyone, without revealing bytes. New weakness with no counterpart
  in the relay design. Must be documented, not discovered.
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
