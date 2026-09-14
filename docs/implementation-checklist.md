# Implementation checklist

Status: **active**
Current work: **cancel and live status** (item 11, phase 4). Receiver consent landed 2026-09-14 under protocol version 2 with `meta_ok` key confirmation. Browser removal (item 8) is done; CI is green on Windows and macOS, and the Windows receiver is fixed (item 10, phases 0–1). Priorities set 2026-09-14; the order is in [`plans/README.md`](plans/README.md#suggested-order-dependencies-not-law)
Last updated: **2026-09-14**

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

**Revised 2026-09-14** at the user's direction: receiver consent, cancel and
live status (item 11), proof of NAT traversal (items 5 and 6), and Windows,
macOS and Linux with transfers between them (item 10). 0.4.0 bundles the browser
removal (item 8), `meta_ok` key confirmation (item 3) and item 11's protocol
change, so users take one wire break, not three. The full dependency order is
in the plans index.

The network lab (item 5) was added 2026-08-31 and runs alongside rather than in
that sequence. It builds nothing the other items depend on; it gives item 3's
unchecked validation gates somewhere to run, so it follows transport and does
not block confirmation.

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
- [x] Phase 2 — `iroh` QUIC transport. 138 tests pass, up from 113. Two peers
      complete a whole encrypted transfer over a direct QUIC connection with no
      Drop server anywhere in it, running the same `send_transfer` and
      `receive_transfer` the relay drives. The control vocabulary is settled by
      [`decisions.md`](decisions.md) entry 12, the framing is written up in
      [`protocol.md`](protocol.md), and two orderings turned out to be
      load-bearing: the sender opens the stream although it accepts the
      connection, and only the sender may close it. **Caveat, and it is not
      small:** both test endpoints bind with relays disabled and meet over
      loopback. Nothing here has exercised a home relay, a NAT, or hole
      punching, which is the feature's whole value proposition. Not an
      oversight and not fixable by trying harder: outbound UDP is blocked on the
      development machine, so every part of this that uses the network is
      unverifiable there. The plan says so under "What cannot be verified on the
      development machine".
- [x] Phase 3 — rendezvous. 145 tests pass. The nameplate-derived keypair, the
      `pkarr` record, publish and resolve, and the address filter from
      [`decisions.md`](decisions.md) entry 14 all exist. The DHT is behind a
      `Directory` trait, so what is tested is everything except the network and
      what is untested is one named struct that says so.
- [x] **The one-guess enforcement, which was the actual gate.** Done 2026-08-29.
      153 tests pass, up from 145. The checkpoint after `meta` exists, the
      sender consumes an attempt on any way of not hearing `meta_ok`, and it
      asks a human before allowing another — `AskTheTerminal`, strict when
      there is no terminal. Which carrier polices guessing is a method on the
      transport trait with no default, because both wrong answers are security
      bugs: `false` on a direct connection is an unlimited guessing oracle, and
      `true` over the relay fails every transfer, since the relay rejects
      receiver frames outside a closed set. The retry loop hands the payload
      back rather than re-reading it, which the type states. **What is not
      proved:** none of it has run over a real network, for the same UDP reason
      as Phases 2 and 3 — the pair tests use an in-memory byte pipe and
      loopback QUIC.
- [x] **Make `meta_ok` provable rather than self-reported.** Done 2026-09-14,
      on both carriers, as protocol version 2 (decisions entry 18). Proposed
      2026-08-31, in
      [`plans/meta-ok-key-confirmation-plan-2026-08-31.md`](plans/meta-ok-key-confirmation-plan-2026-08-31.md).
      The frame above is an unauthenticated assertion made by the party being
      rate-limited, so a wrong guesser can send it anyway: the attempt counter
      never climbs and the human is never prompted. The rate limit holds; the
      *noticing* that entry 13 chose the prompt for does not. Fix is a fourth
      HKDF output under `drop/v1/confirm`, compared in constant time. Land it
      before the direct path ships — after that it is a wire break.
      **Corrected 2026-09-14: that deadline passed.** The direct path, on by
      default, shipped in v0.2.0 (`cli/src/direct.rs` is in that tag), so this
      is now a wire break. It ships in 0.4.0 under the same version bump as
      item 11, so users take one break, not two.
- [x] Phase 4 — selection, automatic fallback, and reporting the path taken.
      Done 2026-08-29. `--transport p2p|relay|auto` (and `DROP_TRANSPORT`),
      defaulting to `auto`; a locally drawn nameplate, since a serverless send
      has nobody to allocate one; and both halves print which path they took.
      Fallback is decided before a code is printed, because the two paths name
      their nameplates differently — the reasoning is in the plan.
- [ ] Phase 5 — documentation, including the DHT address-disclosure weakness

**Gate: met, 2026-08-29, for its first half.** Two CLIs moved 3,000,000 bytes
byte-identical with `DROP_SERVER` pointed at a dead port and `--transport p2p`
forbidding fallback, so no Drop server was in the path. The relay half of the
gate — a UDP-blocked network completing over the relay and saying so — is not
re-testable here any more, because outbound UDP started working on this machine
and `--transport relay` is now the only way to exercise that branch. It passes.

Found by that first run and fixed: the receiver dropped the QUIC endpoint on the
way out of its dial, killing every direct transfer at the moment it started.
Every loopback test passed throughout — they hold both endpoints in scope and
so could not express the bug. Now pinned by
`dropping_an_endpoint_ends_the_transfer_on_it`.

## 4. Receiver preview and confirmation

Plan: [`receiver-confirmation-plan-2026-08-19.md`](plans/receiver-confirmation-plan-2026-08-19.md)
Status: **abandoned — superseded by item 11** on 2026-09-14

Written against cleartext metadata and a browser client. Its findings carry
forward into item 11; its protocol design does not.

## 5. Network topology lab

Plan: [`network-lab-plan-2026-08-31.md`](plans/network-lab-plan-2026-08-31.md)
Status: **active**

A `netlab/` directory that builds the real binaries and runs them inside Linux
network namespaces against constructed topologies — NAT, latency, loss, blocked
UDP — so the peer-to-peer plan's validation gates stop being unreachable by
hand. No part of the Drop protocol is reimplemented there; the lab starts real
binaries and inspects what comes out.

- [x] Phase 0 — machine-readable carrier reporting in the CLI. Done
      2026-08-31. 156 tests, up from 153. `--status` and `DROP_STATUS` add one
      `drop-status: path=... fallback=...` line beside the prose, so a harness
      matches on a stable string rather than on sentences written to be
      reworded. Asserted against the real binary as a subprocess, because
      in-process assertions would leave the flag parsing and the choice of
      stream unchecked — which is precisely what a lab depends on.
- [x] Phase 1 — namespaces, the first topologies, three passing tests. Done
      2026-08-31. `netlab/` runs the real binaries across a router and two
      segments in Linux network namespaces, as an ordinary user: an
      unprivileged user namespace grants `CAP_NET_ADMIN` inside itself, so the
      pytest session re-executes into `unshare -Urnm` and skips cleanly only
      where a kernel refuses. **The phase's stated gate was unmeetable and was
      replaced.** It asked that the UDP-blocked test fail when the `iptables`
      rule is removed; in a lab with no route to the internet the direct path
      cannot be set up either way, so nothing can attribute the fallback to the
      block. The topology honestly shows the fallback firing, completing, and
      being reported — and says so. Two negative controls that do discriminate
      replace the gate: the router is shown to be carrying the transfer, and
      the relay to be relaying it.
- [x] Phase 2 — latency, and the `window / RTT` claim in
      [`protocol.md`](protocol.md). Done 2026-09-01. Throughput is measured at
      200, 400 and 800 ms acknowledgement loops, asserted never to exceed
      `WINDOW_BYTES / RTT` and to halve as the round trip doubles. **Three
      corrections were needed to make the lane measure anything.** The round
      trip that binds the window is the receiver's acknowledgement loop —
      four traversals, not the two the phase assumed — so a halved delay would
      have doubled the ceiling and passed under it without checking anything.
      `measure_rtt` was reading 15-20% high because `ping`'s first packet pays
      for address resolution across the delayed link, and the lane divides by
      that number. And a single transfer's wall clock is mostly handshake at
      these round trips (2.8 s, 5.3 s, 9.6 s of setup), which scales with RTT
      too — so dividing bytes by seconds would have reported the handshake as
      throughput *and still looked inversely proportional*. Rates are now taken
      as a slope across two payload sizes. The lab builds `--release`: debug
      moves 6 MiB/s against 600 MiB/s optimised, below every ceiling under
      test, so the window could never have been the binding constraint.
- [x] Phase 3 — packet loss. Done 2026-09-01. 16 MiB across a router dropping
      1% a hop arrives byte-identical and terminates, with a 5% run recorded
      and not asserted. The deadline is the real assertion — a transfer that
      never finishes and never errors is what a flow-control bug looks like
      from outside — so a timeout became a `Transfer` outcome rather than the
      `LabError` that means the lab itself broke, which is the same conflation
      Phase 1 fixed for a different path. Loss is proved present by reading the
      qdisc back rather than inferred from timing: at zero RTT, 1% loss
      completes as fast as no loss at all.
- [x] Phase 4 — the direct-path topologies. Done 2026-09-10, once **open
      question 1 was answered** by making rendezvous configurable — a deployment
      feature in its own right, item 6 below and
      [`decisions.md`](decisions.md) entry 15. The lab runs an `iroh-relay`
      server and a three-node `mainline` testnet in one namespace and points
      `drop` at them. Three topologies: a plain LAN with no `api` process
      anywhere, asserted by `pgrep` before and after; both peers behind a
      port-preserving NAT; both behind `--random-fully`.
      **Two of the plan's own assertions for this phase were wrong and were
      replaced.** `drop --status` reports `path=p2p` whenever no Drop server was
      involved, which is true whether iroh punched through the NAT or carried
      the connection over its relay — so asserting it proved nothing about
      traversal, and the symmetric row's expected `fallback=rendezvous` never
      fires at all, because rendezvous succeeds and the Drop relay is never
      consulted. A hole punch is measured on the wire instead, by counting bytes
      on the rendezvous host's isolated link, and the NAT's mapping behaviour is
      measured directly — one socket, two destinations, compare the source ports
      the far end saw — rather than inferred from the `iptables` rule, because a
      misbuilt symmetric NAT passes a test asserting the punch failed.
      Separately: **the lab's skip path was broken on any Ubuntu 24.04 or
      later**, where `kernel.apparmor_restrict_unprivileged_userns` refuses the
      namespace while the two sysctls the probe actually read both say yes. A
      wrong yes was unrecoverable because the caller `execvp`s, so the whole run
      exited with one line of `unshare` error and no test report. The probe now
      attempts a namespace instead of predicting one.
- [ ] Phase 5 — dated report under [`validation/`](validation/) and a separate
      CI workflow, nightly and label-triggered, never blocking pull requests

- [ ] **Found 2026-09-14: hole punching has never been exercised here.** The
      peers' QUIC address discovery fails TLS against the lab helper's
      self-signed certificate (`invalid peer certificate: UnknownIssuer`), so
      neither learns its public address (`global_v4: None`) and no punch is
      ever attempted. A throttled 48 MiB run showed the payload on the
      rendezvous link for all 27 seconds, and conntrack on both NATs showed
      peers dialling each other's *private* addresses only. The full-cone
      failure is this, not iroh failing to punch. The helper's comment and the
      rendezvous plan's risk entry both claimed iroh skips that verification,
      and neither is true for 1.0.3. The fix needs a decision: item 6 phase 3.
      Separately, `authentication failed` still appears intermittently (2 of 4
      runs today) and is unexplained.

Gate: every topology fails when its defining condition is removed, demonstrated
once per topology and recorded. A lab that passes either way is measuring
nothing, which is the failure the peer-to-peer plan's loopback tests already
document about themselves. Outstanding for `udp_blocked` alone, which was
recorded as unmeetable at Phase 1 and became possible at Phase 4: attributing a
fallback to a cause needs the direct path to be able to succeed when the cause is
absent, and now it can.

## 6. Self-hosted rendezvous

Plan: [`self-hosted-rendezvous-plan-2026-09-10.md`](plans/self-hosted-rendezvous-plan-2026-09-10.md)
Status: **phases 1 and 2 done; phase 3 awaiting a decision**

`DROP_RENDEZVOUS_RELAY` and `DROP_RENDEZVOUS_BOOTSTRAP` point the direct path at
an iroh relay and DHT nodes a deployment runs itself, instead of n0's relays and
the public mainline routers compiled in. Unset, nothing changes.

- [x] Phase 1 — the two values. Done 2026-09-10.
      [`decisions.md`](decisions.md) entry 15 and a section in
      [`security.md`](security.md) record what an operator takes on: a relay they
      name sees connection metadata, a bootstrap node they name can refuse to
      store a record, and neither can lead a receiver to the wrong peer because a
      rendezvous record was never evidence of identity. **A malformed value is an
      error rather than a silent return to the public default**, which is the
      load-bearing decision here: an operator who meant to keep rendezvous inside
      their network and quietly got the public DHT has lost exactly what they
      configured, invisibly. That is also why the relay URL's scheme is checked —
      `RelayUrl::from_str` is `Url::from_str`, so `relay.example:3340` parses
      happily into a URL whose scheme is `relay.example`, binds without
      complaint, and then spends `ONLINE_TIMEOUT` reaching no relay at all.
- [x] Gate: two `drop` processes complete a direct transfer against a relay and
      DHT on loopback with nothing public reachable, and the same transfer fails
      when that infrastructure is stopped. The negative control is the half that
      matters — this machine can reach the real DHT, so a passing transfer alone
      would not show which one carried the rendezvous.

- [ ] Phase 3 — proposed 2026-09-14, **needs the user's decision** because it
      changes what the CLI trusts. A relay with a certificate from a private CA
      silently gets no address discovery, so no hole punching: transfers still
      complete over the relay and `--status` still says `path=p2p`. Either
      `DROP_RENDEZVOUS_CA` adds an operator's CA for the rendezvous relay only,
      or the docs say traversal needs a publicly trusted certificate. Both come
      with a warning when a custom relay yields no discovered address. The lab
      needs the former to prove traversal at all.

Why this is a feature and not a knob added for a test is argued in the plan and
in entry 15: a self-hoster can already run their own relay, but rendezvous was
compiled in, so the direct path could not work at all inside an egress-filtered
network. The network lab above is the first consumer rather than the reason.

## 7. Interactive terminal UI

Plan: [`interactive-terminal-ui-plan-2026-09-10.md`](plans/interactive-terminal-ui-plan-2026-09-10.md)
Status: **phases 0 and 1 done, phase 2 partly**

`drop send` and `drop recv`, typed bare on a terminal, open a small full-screen
interface: a file browser, a checkbox options screen, a code field, a
destination picker, progress, and a close on both sides when the transfer ends
or is cancelled. The flags stay and become the program-facing surface.

- [x] Phase 0 — activation rule. Done 2026-09-10. All three streams must be
      terminals, stdout included, because the transfer code goes there so it
      survives a pipe; `DROP_STATUS` counts even with a bare command line; and
      **any** flag means the command, rather than a curated list of flags that
      would be wrong the first time one was added. 180 tests pass against 171
      before, the nine new ones being this rule. The `netlab` half of the gate
      came back 7 passed, 1 failed — `test_a_full_cone_nat_is_punched_through`,
      which belongs to item 5's phase 4 and reproduces three runs out of three
      while the other two direct-path topologies pass. **No `HEAD` baseline
      exists** for it: the lab is uncommitted and depends on uncommitted
      `DROP_RENDEZVOUS_*` support, so the gate is satisfied for seven tests and
      inconclusive for the eighth.
- [x] Phase 1 — terminal lifecycle. Done 2026-09-10. A guard plus a free
      `restore()` over an atomic, so the signal handler — which owns no guard
      and does not unwind — can call it too. The terminal is given back
      *before* the spool file is deleted, the opposite of what the plan first
      said: both are a few syscalls and the unrecoverable one goes first.
      `recv` has a termination handler for the first time. **Ctrl-C had to
      become a key as well as a signal**, because raw mode stops the driver
      turning it into SIGINT.
- [~] Phase 2 — the screens. Chooser, file browser, options and code entry are
      done and drive real transfers; the transfer screen itself is phase 3, so
      the interface currently closes and the transfer prints what it always
      has. The options screen offers *adding* a custom relay rather than
      showing a URL field, warns when the relay carrier is chosen without one,
      and shows a relay already in `DROP_SERVER` as in effect — nothing added
      means no relay, per [`decisions.md`](decisions.md) entry 16.
- [ ] Phase 3 — progress routed through the interface, and closing. The
      receiver has no cancel path today: the sender sends `{"type":"cancel"}`
      on a stream failure and acts on a `cancelled` status, but nothing in
      `recv.rs` sends one, so "closes for both sides when cancelled" is
      currently only true in one direction.
- [ ] Phase 4 — [`decisions.md`](decisions.md) entry 13's approval prompt as a
      screen, counter intact, unattended behaviour unchanged.
- [ ] Phase 5 — help and docs.

Two dependencies are added, ratatui and crossterm, into a manifest that
justifies every entry it has. Both are pure Rust, which is the standing bar for
the four prebuilt targets. Release binary went 26,988,848 → 27,482,144 bytes,
**+493 KB (+1.8%)**, so the crossterm-only fallback is not needed.

## 8. Browser client removal

Plan: [`browser-client-removal-plan-2026-09-11.md`](plans/browser-client-removal-plan-2026-09-11.md)
Status: **done** 2026-09-14, recorded as [`decisions.md`](decisions.md) entry 17

Found on the way and fixed: `Dockerfile` has not built since the envelope became
its own crate (it never copied `crypto/`). **Needs a settings change when this
merges:** branch protection requires "Web", which no longer reports, and
"Analyze JavaScript and TypeScript", which is now "Analyze Rust".

Delete `web/` and `crypto-wasm/` and every dependent — the server routes that
serve the client, the CI job that builds it, the Docker stage that bundles it,
and the documentation that describes it. **The relay stays**: entry 16 removed
the browser's reason for it, not the UDP-blocked network's, and `netlab` covers
that one.

- [x] Phase 0 — move `install.sh` out of `web/public/`. Done 2026-09-14:
      now `scripts/install.sh`, byte-identical to the v0.3.0 asset. **Release-critical and
      lands alone.** `release.yml` sparse-checks it out at line 130 and
      publishes it at 153 under `fail_on_unmatched_files: true`, so deleting
      `web/` first fails the next tag in `publish`, after the whole build matrix
      has already succeeded.
- [x] Phase 1 — the server stops serving a browser: the `/` and `/assets`
      routes, and the `index_serves_the_drop_entrypoint` test that asserts the
      Svelte entrypoint.
- [x] Phase 2 — delete `web/` and `crypto-wasm/`, the workspace member, and
      `.cargo/config.toml`'s wasm32 rustflag.
- [x] Phase 3 — the `web` CI job, `Dockerfile.fullstack`, and the ignore-file
      entries.
- [x] Phase 4 — documentation across nine files, `decisions.md` entry 17, and
      entry 11 marked superseded rather than deleted.

Gate: the workspace is green with two members rather than three, a CLI-to-CLI
transfer over `--transport relay` still completes, and a release built from the
resulting tree publishes `install.sh` from its new path. The third cannot be
checked by running tests and has to be read against the workflow before tagging.

Open questions answered 2026-09-14, all as the plan leaned: remove
`DROP_ALLOWED_ORIGINS`; `GET /` answers a plain-text 404 naming the project;
the browser claim rules in `AGENTS.md` stay, reworded as dormant; ships in 0.4.0
with `meta_ok` confirmation and item 11. The old `chore/remove-web` branch
deletes `install.sh` first, which is the exact thing phase 0 exists to prevent,
so it is not the base for this work.

## 9. Browser client on iroh

Plan: [`browser-on-iroh-plan-2026-09-11.md`](plans/browser-on-iroh-plan-2026-09-11.md)
Status: **proposed — not scheduled**

Rebuild the browser client as an iroh node compiled to WebAssembly, speaking the
same conversation the CLI speaks, so the relay stops translating between two
dialects — it renames and invents control frames today, which is item 3 phase
1's finding and the last consumer of that is the browser.

Recorded so it is not rediscovered as new. **Nothing here is committed work**,
and open question 1 — whether a browser client that can never be direct is worth
its maintenance — should be answered before any of it is started.

- [ ] Blocker 1 — `Transport`'s futures are all declared `Send` and wasm futures
      are not. Wants n0-future's conditional-`Send` approach, and it is a
      refactor of shipped tested code for the benefit of code that does not
      exist yet.
- [ ] Blocker 2 — `discovery-pkarr-dht` cannot run in a browser. Rendezvous
      needs pkarr over HTTP relay, which `DROP_RENDEZVOUS_BOOTSTRAP`'s
      `host:port` shape does not describe, and which has to inherit entry 15's
      no-silent-fallback rule somewhere a user cannot read an error.
- [ ] Blocker 3 — `peers_enforce_one_guess` needs a third answer, since a
      browser has neither a Drop relay refusing a second claim nor a terminal to
      ask. A decisions entry, not a plumbing choice.
- [ ] Blocker 4 — item 3's `meta_ok` confirmation lands first, or the wire
      breaks twice.
- [ ] Blocker 5 — `send::run` and `recv::run` spool and write to a filesystem a
      browser does not have. The middle of the stack is shared; both ends are
      per-platform.

A browser peer is **permanently relayed** — iroh cannot hole-punch from a
sandbox, and WebTransport and WebRTC are both unimplemented there — so a browser
transfer is never the direct path and must never be described as one.

## 10. Windows, macOS and Linux

Plan: [`cross-platform-plan-2026-09-14.md`](plans/cross-platform-plan-2026-09-14.md)
Status: **active** — phases 0 and 1 done

Install and run on all three, and a file or folder sent between any two of them
arrives intact or says exactly what could not be reproduced. **Today: Linux
works, macOS is built but has never had a test run on it, Windows has no
build.** CI runs on Ubuntu only, so no `cfg(not(unix))` branch has ever
compiled.

- [x] Phase 0 — the Rust job on `ubuntu-24.04`, `macos-14` and `windows-2025`,
      and record what fails before fixing any of it. Done 2026-09-14. macOS
      passed first time. On Windows, only two Unix-only test helpers failed
      Clippy; the whole suite passed once they were gated, and every non-Unix
      branch in the CLI compiled for the first time ever. One macOS-only race
      in a relay test was fixed. The new checks are not required yet (a
      settings change).
- [x] Phase 1 — done 2026-09-14. A Windows receiver: a symlink it cannot create is a warning,
      not an abort (today it aborts every Linux-to-Windows folder with a
      symlink in it); names Windows reads differently (`a:b` is an NTFS stream,
      `CON` a device, `report.` loses its dot) are rewritten with a warning;
      an uncreatable entry on any platform warns and continues.
- [ ] Phase 2 — a Windows sender: portable symlink targets, spool cleanup when
      the console is closed, VT processing for the progress line.
- [ ] Phase 3 — `x86_64`/`aarch64-pc-windows-msvc` release zips with a static
      CRT, `install.ps1`, README install lines.
- [ ] Phase 4 — archives produced on each OS and extracted on the others in CI,
      nine pairings.
- [ ] Phase 5 — manual Windows ↔ Linux checklist on the user's machine, both
      carriers, the interface, Ctrl-C, closing the window, the firewall dialog.
- [ ] Phase 6 — documentation and a decision entry for the Windows name policy.

Gate: CI green on three OSes; hostile Windows names contained and honest ones
rewritten with warnings; Windows assets install; nine archive pairings green; the
manual checklist recorded.

## 11. Receiver consent, cancel, and live status

Plan: [`receiver-consent-and-status-plan-2026-09-14.md`](plans/receiver-consent-and-status-plan-2026-09-14.md)
Status: **active** — phases 0–3 done 2026-09-14 (decisions entry 19); phase 4, a person cancelling and the sender's state lines, next

The receiver sees name, type, size and where it will be saved, and accepts
before a byte is written. Either side can cancel and the other is told in words.
The sender sees the receiver connect, pass the code, review, accept or decline,
receive, finish.

- [x] Phase 0 — decisions entry; consent before bytes, `--yes` required without
      a terminal (user decision 2026-09-14), reasons as enumerations, version 2,
      exit codes.
- [x] Phase 1 — display sanitisation, alone. **Live bug**, fixed 2026-09-14: a
      received filename reached `eprintln!` unfiltered, so an escape sequence
      in it could redraw the terminal. Also covered: error messages from a peer
      or the relay, archive warnings, and the file browser. Pinned end to end
      through the binary, with a negative control.
- [x] Phase 2 — protocol and relay: `meta_ok` on both carriers, `accept`,
      `decline`, receiver `cancel`, `finishing`; the relay refuses chunks
      before `accept` and stops counting declines and cancels as failures;
      `ENVELOPE_VERSION` and `DROP_ALPN` to 2.
- [x] Phase 3 — receiver consent in the CLI: destination planned but not
      created, preview with a receiver-derived type and a program warning,
      120 s deadline, refusal without a terminal before connecting.
- [ ] Phase 4 — cancel through both transfer paths, two-stage Ctrl-C, sender
      state lines and `drop-status: state=`, exit codes 3 and 4.
- [ ] Phase 5 — review and transfer screens with Accept/Decline and Cancel,
      with item 7 phase 3.
- [ ] Phase 6 — documentation; release notes lead with `--yes`.

Gate: declining leaves the destination unchanged and the sender exits 3; a
hostile filename renders inert everywhere; either side's cancel reaches the other
in words on both carriers; no terminal without `--yes` refuses before contacting
anything; 0.3.0 against 0.4.0 fails with a sentence, not a hang.

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
