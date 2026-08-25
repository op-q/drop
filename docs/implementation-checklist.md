# Implementation checklist

Status: **active**
Current work: **[peer-to-peer transport](plans/peer-to-peer-transport-plan-2026-08-20.md)** — Phases 1 and 2 done, Phase 3 blocked on a security question
Last updated: **2026-08-24**

The tactical view of what is being built and what state it is in. The detailed
reasoning, risks, and validation for each item live in its plan under
[`plans/`](plans/README.md) — this file mirrors their status, it does not
duplicate them.

Update checkboxes in place: `[ ]` pending, `[x]` verified complete. An item is
complete when its evidence exists, not when the code compiles.

## How to use this checklist

1. Read the item's plan before starting. Verify its file and line references
   against the current source; a plan records intent at writing time.
2. Work one item at a time on a focused topic branch.
3. Include tests for behavior changes in the same commit as the behavior.
4. Run the full validation set from [`commands.md`](commands.md).
5. Update the plan's checkboxes and this mirror before finishing.

## Work order

Teardown, then encryption, then peer-to-peer transport, then confirmation.
Reordered 2026-08-20 at the user's direction: encryption moved ahead of
confirmation, and transport was added. The reasoning and the cost of the swap
are in
[`plans/README.md`](plans/README.md#suggested-order-dependencies-not-law).

## 1. Relay teardown reset

Plan: [`relay-teardown-drain-plan-2026-08-19.md`](plans/relay-teardown-drain-plan-2026-08-19.md)
Status: **done**

A sender intermittently sees `Connection reset by peer` after a transfer that
actually succeeded. The upload receive task stops reading the socket when the
sender completes, so the peer's closing handshake reply lands unread and the
socket is dropped with data queued, producing RST instead of FIN.

- [x] Phase 1 — keep the receive task alive through teardown
- [x] Phase 2 — bound the drain, in two stages rather than one
- [x] Phase 3 — check the receiver socket for the same shape
- [x] Phase 4 — repeated-run evidence

Gate met: 80 runs clean, against 3 failures in 30 on unfixed `main`.

## 2. End-to-end encryption

Plan: [`end-to-end-encryption-plan-2026-08-19.md`](plans/end-to-end-encryption-plan-2026-08-19.md)
Status: **done**

The relay forwards bytes it cannot read. Both peers derive the key from the
short code by SPAKE2; it never crosses the wire.

- [x] Phase 0 — decided 2026-08-20: AES-256-GCM, SPAKE2 key agreement, and the
      narrower claim that separates the CLI case from the browser case.
      Recorded in [`decisions.md`](decisions.md) entry 7.
- [x] Phase 1 — envelope: wordlist codes, handshake, HKDF, chunk framing, AAD,
      metadata blob, version. Transport-independent by requirement.
      25 tests; tamper, reorder, truncate, duplicate, and wrong code all
      covered at the envelope level.
- [x] Phase 2 — relay carries the envelope; no plaintext in logs. Cleartext
      `meta` is now version, sealed size, and an opaque blob; `Session` has no
      filename field at all.
- [x] Phase 3 — CLI: encrypt, decrypt, distinct failure modes, and a partly
      written file removed rather than left looking whole. Landed with Phase 2,
      because a breaking protocol change has no green intermediate state.
- [x] Phase 4 — web: the envelope compiled to WebAssembly rather than
      reimplemented, so both clients run one implementation. Recorded in
      [`decisions.md`](decisions.md) entry 11. Interoperation with the CLI is
      covered in both directions against a real relay by
      `web/tests/interop.test.mjs`.
- [x] Phase 5 — documentation. `protocol.md`, README, `security.md`, and the
      AGENTS.md invariant now separate the CLI case from the browser case.

Gate met: tampering, reordering, and truncation are all detected; a wrong code
fails cleanly and burns the session; no plaintext filename reaches logs or
`/metrics`; and the CLI and the browser envelope open each other's transfers
over a real relay.

Not covered: `App.svelte` itself. `tsc` does not check `.svelte` files and the
interop tests drive the envelope and the protocol from Node, not the UI. The
browser flows have been exercised by the build and by their shared envelope,
not by a browser.

Found after the phases closed and fixed on 2026-08-24: a receiver that claimed
its socket before the sender deadlocked the transfer, because it sent its half
of the key exchange on connect and the relay drops a half that has no peer to
go to. Both receivers now reply to the sender's half instead, the relay refuses
an early half rather than dropping it silently, and the connection order is
pinned by a test rather than raced. Detail in the plan.

## 3. Peer-to-peer transport

Plan: [`peer-to-peer-transport-plan-2026-08-20.md`](plans/peer-to-peer-transport-plan-2026-08-20.md)
Status: **active**

Two CLIs connect directly over QUIC and find each other through a mainline-DHT
record derived from the code, so a transfer needs no Drop-operated server. The
relay stays as an untrusted fallback for browsers and for networks where this
cannot work. Recorded in [`decisions.md`](decisions.md) entry 10.

- [x] Phase 1 — transport abstraction, existing WebSocket path moved behind it.
      113 tests pass, up from 104; the new ones drive the transfer paths over a
      second implementation of the trait that is not a socket. Two findings in
      the plan: establishing a connection is deliberately not on the trait, and
      the relay turns out to rename and invent control frames, so Phase 2 has
      to decide who does that over a direct connection.
- [x] Phase 2 — `iroh` QUIC transport. 132 tests pass, up from 113. Two peers
      complete a whole encrypted transfer over a direct QUIC connection with no
      Drop server anywhere in it, running the same `send_transfer` and
      `receive_transfer` the relay drives. The control vocabulary is settled by
      [`decisions.md`](decisions.md) entry 12, the framing is written up in
      [`protocol.md`](protocol.md), and two orderings turned out to be
      load-bearing: the sender opens the stream although it accepts the
      connection, and only the sender may close it. **Caveat, and it is not
      small:** both test endpoints bind with relays disabled and meet over
      loopback. Nothing here has exercised a home relay, a NAT, or hole
      punching, which is the feature's whole value proposition.
- [ ] Phase 3 — rendezvous: nameplate-derived keypair (done), `pkarr` publish
      and resolve. **Blocked on a security question the derivation surfaced:**
      the one-guess property that makes 33 bits defensible was enforced by the
      relay refusing a second claim, and a serverless transfer has no relay to
      do it. Without an answer the direct path is an unlimited online oracle.
      Proposed answer and the two details it needs are in the plan under
      "The unsolved problem". The QUIC path must not be reachable by default
      until it is settled.
- [ ] Phase 4 — selection, automatic fallback, and reporting the path taken
- [ ] Phase 5 — documentation, including the DHT address-disclosure weakness

Gate: two CLIs transfer with no Drop server reachable; a UDP-blocked network
still completes over the relay and says so.

## 4. Receiver preview and confirmation

Plan: [`receiver-confirmation-plan-2026-08-19.md`](plans/receiver-confirmation-plan-2026-08-19.md)
Status: **proposed — needs revision**

Moved behind encryption on 2026-08-20. Its plan is written against a cleartext
`meta` that encryption seals, so it must be revised before it is implemented,
not followed as written.

The receiver sees name, size, type, and destination, and answers y/n before any
bytes move.

- [ ] Phase 1 — protocol: accept/decline, `receiver_accepted`, decline as a
      normal outcome, accept deadline
- [ ] Phase 2 — safe rendering of peer-supplied names
- [ ] Phase 3 — CLI prompt, ahead of destination creation, plus `--yes`
- [ ] Phase 4 — web confirmation step
- [ ] Phase 5 — decide whether `Meta` gains a file count

Gate: declining leaves nothing on disk and is not counted as a failure; a
filename carrying escape sequences renders inert; a non-TTY without `--yes`
fails clearly.

## Not scheduled

Recorded so they are not rediscovered as new ideas. None are committed work.

- **Resume and retry** after a disconnect. The honest design is receiver-side:
  the receiver keeps its partial file and asks for a byte offset on reconnect.
  Server-side resume would require the relay to hold data across the gap, which
  [`decisions.md`](decisions.md) entry 1 rules out.
- **Horizontal scaling.** Needs shared session coordination and transfer-aware
  routing; session affinity alone cannot recover a live WebSocket.
- **The download socket's teardown**, which carries the same shape the upload
  socket had: its receive task stops reading before the send task writes a
  `Close`. It produces no user-visible symptom, because the receiving client
  discards the result of its own close, so it is latent rather than harmless.
  Fixing it needs a way to show the change worked, which the upload side had
  and this side does not. See the Phase 3 finding in
  [`plans/relay-teardown-drain-plan-2026-08-19.md`](plans/relay-teardown-drain-plan-2026-08-19.md).
- **`receiver disconnected` is the sender's message for any receiver-side
  failure.** The receiver returns an error and drops the socket, so the relay
  can only report a disconnect — the sender learns nothing about what actually
  went wrong. Numbering colliding filenames removed the most common trigger,
  but the message is still misleading for every other receiver-side failure.
  The fix is for the receiver to send an `error` control frame before closing;
  it overlaps with the decline path in
  [`plans/receiver-confirmation-plan-2026-08-19.md`](plans/receiver-confirmation-plan-2026-08-19.md),
  which also needs the sender to distinguish outcomes it currently cannot.
- **The published binary reports the wrong version.** `v0.1.1` shipped while
  `version` in `Cargo.toml` still reads `0.1.0`, so `drop --version` disagrees
  with the tag it was built from. Worth a version bump plus a release-workflow
  check that the tag and the manifest match, since this recurs every release.
- **Prometheus text** from `/metrics`, which currently returns a JSON snapshot.
- **A first transfer shakeout run** using
  [`validation/transfer-shakeout-template.md`](validation/transfer-shakeout-template.md).
  Worth doing before the next release regardless of the three items above.
