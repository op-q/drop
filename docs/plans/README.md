# Plans

Full plans with checkbox progress. These files preserve the entire plan
context — phases, file lists, risks, validation steps, kickoff prompts, and
open questions — so work can resume on another machine with no chat history.

The binding working rules live in [AGENTS.md](../../AGENTS.md). This file makes
the plan contract unmissable and indexes what is here.

## The plan contract

1. **Write plans in full.** When a multi-step plan is created, save the entire
   plan as `docs/plans/<topic>-plan-YYYY-MM-DD.md` before or as implementation
   starts. Never compress a plan to a summary. If a decision took investigation
   to reach, the investigation belongs in the plan, not in the commit message.
2. **Update checkpoints while working.** Flip `[ ]` to `[x]` as work lands, add
   short inline notes for blockers or scope changes, and update the
   `Last updated:` line on material changes.
3. **Mirror, do not duplicate.** Mirror status into
   [`../implementation-checklist.md`](../implementation-checklist.md), which
   stays the tactical view. The plan is the detailed source of truth.
4. **Verify before building.** A plan records intent at writing time; the
   repository may have moved. Check a plan's context claims — especially file
   and line references — against the source before building on them, and fix
   the plan if it drifted.
5. **Status header.** Every plan carries
   `Status: proposed | active | done | abandoned`. Mark an abandoned plan
   rather than deleting it, with one line saying why.
6. **Promote durable decisions.** When a plan settles something costly to
   reverse, move it into [`../decisions.md`](../decisions.md) and mark the plan
   done. Plans are working documents; decisions outlive them.

## Active

- [`peer-to-peer-transport-plan-2026-08-20.md`](peer-to-peer-transport-plan-2026-08-20.md)
  — connect the two CLIs directly over QUIC with `iroh`, finding each other
  through a mainline-DHT record derived from the nameplate, so a transfer needs
  no Drop-operated server. Phase 1 landed 2026-08-24: the transfer paths are
  written against a transport trait and the WebSocket client sits behind it.
  Phase 2 is the QUIC transport, and starts with a decision the refactor
  surfaced — the relay renames and invents control frames, and a direct
  connection has nobody to do that.
- [`browser-client-removal-plan-2026-09-11.md`](browser-client-removal-plan-2026-09-11.md)
  — **active 2026-09-14**, open questions answered; phase 0 is next and lands
  alone. Delete `web/` and `crypto-wasm/` and every dependent, keeping the relay.
  [`../decisions.md`](../decisions.md) entry 16 removed the browser client's
  audience: with no hosted relay and no compiled-in default, only someone
  self-hosting the fullstack image can reach it. Phase 0 stands alone and is
  release-critical — `release.yml` publishes `web/public/install.sh`, so the
  script moves to `scripts/` before anything is deleted or the next tag fails
  in `publish` after a full build matrix. The relay is explicitly not in scope:
  its browser justification is gone, its UDP-blocked one is real and `netlab`
  covers it.
- [`network-lab-plan-2026-08-31.md`](network-lab-plan-2026-08-31.md)
  — **2026-09-14 finding:** hole punching has never been exercised in the lab.
  QUIC address discovery fails TLS against the lab's self-signed helper
  (`UnknownIssuer`), so no peer learns its public address and no punch is
  attempted. The full-cone row fails for that reason, not because iroh cannot
  punch. The same gap hits a self-hosted relay with a private-CA certificate;
  see the self-hosted rendezvous plan's phase 3. Background:
  a `netlab/` directory that runs the real binaries inside Linux network
  namespaces against constructed topologies, so the peer-to-peer plan's
  validation gates stop being unreachable by hand. Two findings shape it: the
  lab needs no root, because an unprivileged user namespace grants
  `CAP_NET_ADMIN` inside itself; and the direct path cannot run hermetically as
  the code stands, because rendezvous needs the public DHT, n0's relays, and an
  address that [`../decisions.md`](../decisions.md) entry 14 deliberately
  refuses to publish. The relay-path topologies are unblocked and come first.
- [`self-hosted-rendezvous-plan-2026-09-10.md`](self-hosted-rendezvous-plan-2026-09-10.md)
  — phases 1 and 2 shipped in 0.3.0. Phase 3, proposed 2026-09-14 and awaiting
  the user's decision: trust an operator's CA for the rendezvous relay, or
  document that traversal needs a publicly trusted certificate.

## Proposed

- [`receiver-consent-and-status-plan-2026-09-14.md`](receiver-consent-and-status-plan-2026-09-14.md)
  — the receiver sees name, type, size and destination and accepts before a
  byte is written; either side can cancel and the other is told in words; the
  sender sees the receiver's state throughout. Supersedes the 2026-08-19
  confirmation plan. Three findings: a received filename can put escape
  sequences on the terminal **today**, so display sanitisation lands first and
  alone; Ctrl-C tells the peer nothing and the relay counts a cancel as a
  failure; and the sender's progress is already acknowledgement-driven, so
  "how much the receiver has" needs no new frame. A wire change, bumping
  `ENVELOPE_VERSION` and `DROP_ALPN` to 2 together with `meta_ok` confirmation,
  so 0.3.0 against 0.4.0 fails with a sentence instead of a hang.
- [`cross-platform-plan-2026-09-14.md`](cross-platform-plan-2026-09-14.md)
  — Windows, macOS and Linux, and transfers between them. Today Linux is the
  only platform tests have ever run on, macOS is built but untested, and
  Windows has no build. Phase 0 puts the test suite on all three runners first,
  because every `cfg(not(unix))` branch has never compiled. Findings from
  reading: a symlink in a folder aborts every Linux-to-Windows folder transfer;
  names like `a:b`, `CON` and `report.` mean something else to Windows (an NTFS
  stream, a device, a stripped dot); closing the Windows console leaves spool
  files in `%TEMP%`. Cross-OS confidence comes from archives produced on each OS
  and extracted on the others in CI, plus a manual Windows ↔ Linux checklist on
  the user's machine. The wire itself is platform-neutral.

- [`browser-on-iroh-plan-2026-09-11.md`](browser-on-iroh-plan-2026-09-11.md)
  — **not scheduled.** Rebuild the browser client as an iroh node compiled to
  wasm, so a browser speaks the same conversation as the CLI and the relay
  stops translating between two dialects. Recorded so it is not rediscovered as
  new. Blocked on three things: the transport trait declares every future
  `Send` and wasm futures are not, `discovery-pkarr-dht` cannot run in a
  browser so rendezvous needs an HTTP relay the entry 15 variables do not
  describe, and `meta_ok` must land first or the wire breaks twice. A browser
  peer is permanently relayed — iroh cannot hole-punch from a sandbox — so it
  is never the direct path.
- [`interactive-terminal-ui-plan-2026-09-10.md`](interactive-terminal-ui-plan-2026-09-10.md)
  — make `drop send` and `drop recv` the only two things a person needs to
  know: typed bare on a terminal each opens a small full-screen interface, and
  every flag stays reachable for programs. Three findings shape the order —
  the code-announce callback is already the right seam, `progress.rs` is not
  and will draw over the screen, and the sender's signal handler exits without
  unwinding, so a raw terminal is never restored on Ctrl-C. Terminal lifecycle
  therefore lands before a single screen is drawn. Bare `drop` opens a chooser
  on a terminal, and still prints usage and exits 1 anywhere else.
- [`meta-ok-key-confirmation-plan-2026-08-31.md`](meta-ok-key-confirmation-plan-2026-08-31.md)
  — make the receiver prove it opened the sealed metadata instead of saying so.
  `meta_ok` carries a key-confirmation value derived from the agreed secret and
  compared in constant time, closing the case where an attacker who guessed
  wrong claims success and so suppresses the prompt entry 13 exists to raise.

## Abandoned

- [`receiver-confirmation-plan-2026-08-19.md`](receiver-confirmation-plan-2026-08-19.md)
  — superseded 2026-09-14 by the receiver consent and status plan. Written
  against cleartext metadata and a browser client; its three findings carried
  forward, its protocol design did not.

## Done

- [`end-to-end-encryption-plan-2026-08-19.md`](end-to-end-encryption-plan-2026-08-19.md)
  — completed 2026-08-21: payloads are sealed with AES-256-GCM under a key both
  peers derive from the code by SPAKE2, so the relay carries an envelope it
  cannot read. The code is split into a public nameplate the relay routes on
  and three secret words that are the PAKE password — a correction made during
  implementation, because the first design handed the relay the password and so
  let it sit in the middle. The browser runs the same envelope compiled to
  WebAssembly rather than a second implementation. Recorded in
  [`../decisions.md`](../decisions.md) entries 7 and 11. Not covered:
  `App.svelte`, which is exercised by its build and its shared envelope rather
  than by a browser.
- [`relay-teardown-drain-plan-2026-08-19.md`](relay-teardown-drain-plan-2026-08-19.md)
  — completed 2026-08-19: the upload receive task now stays alive through
  teardown and answers the sender's closing handshake, so a socket is no longer
  dropped with the peer's reply unread. 80 runs clean against 3 failures in 30
  before. Two findings recorded rather than fixed: the assertions #30 removed
  stay out because they guard nothing under per-transfer relay isolation, and
  the download socket carries the same latent shape without a user-visible
  symptom.

## Suggested order (dependencies, not law)

Teardown first — **done**. It was a real defect, it needed no protocol change,
and the confirmation feature adds a new terminal close path, decline, that
would have inherited the same reset bug on the day it shipped.

Confirmation next. It is reviewable without crypto, and it settles what the
receiver sees and when.

**Revised 2026-08-20.** Encryption was moved ahead of confirmation at the
user's direction, and peer-to-peer transport was added after it. The order is
now: encryption, then transport, then confirmation.

The cost of the swap is real and was accepted knowingly. Confirmation would
have settled what the receiver sees before encryption relocated those fields;
doing encryption first means the confirmation prompt must be designed against
metadata that is already sealed, and its plan will need revisiting rather than
implementing as written.

Transport follows encryption because the envelope is what makes a relay
untrusted, and an untrusted relay is what makes falling back to one acceptable.
Building the QUIC path first would have produced a fast path with no honest
story for the slow one.

Confirmation last is otherwise unchanged, and no longer carries the caveat that
it ships while the relay can forge the filename it displays.

**Revised 2026-09-14**, at the user's direction. The priorities are now receiver
consent, cancel on both sides, and the sender seeing the receiver's state; proof
of NAT traversal; and Windows, macOS and Linux with transfers between them. The
order that serves them, by dependency:

1. **Browser removal phase 0**, alone. Release-critical, and every later plan
   edits `release.yml` or `ci.yml` after it.
2. **Cross-platform phase 0** (tests on three OSes). Cheap, and every later
   change is then checked on Windows as it lands instead of all at once at
   the end.
3. **Browser removal phases 1–4.** Deletes the `meta_ok` plan's browser phase and
   the second protocol implementation that would otherwise need the new frames.
4. **Consent plan phase 1** (display sanitisation). A live bug and no wire
   change.
5. **`meta_ok` key confirmation, then consent phases 2–4.** One wire bump, to
   version 2.
6. **Cross-platform phases 1–3** (Windows receiver, sender, build and install),
   then **consent phase 5** with the interface's transfer screen.
7. **Tag 0.4.0.**
8. Then the NAT proof, once self-hosted rendezvous phase 3 is decided, and
   cross-platform phases 4–5.
