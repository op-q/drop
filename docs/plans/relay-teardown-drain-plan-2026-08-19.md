# Relay teardown drain plan

Status: **done**
Created: **2026-08-19**
Last updated: **2026-08-19**
Completed: **2026-08-19**

## Goal

A sender that completes a transfer successfully never sees a connection error.
Today it intermittently sees `error: IO error: Connection reset by peer` even
though the file arrived intact and the receiver confirmed it.

## Context

This is a pre-existing defect, not a regression. It was diagnosed while fixing
CLI test flakiness in PR #30 and deliberately left out of that change, because
altering production shutdown semantics does not belong in a test-stability fix.
PR #30's description records it as a follow-up.

Its visible symptom before that fix: roughly one run in six of the two small
payload CLI transfer tests failed with `Connection reset by peer`, reported
against the sender while the receiver had already reported success and written
correct files. PR #30 made the tests stop asserting the completion handshake
and gave each transfer its own relay, which removed the flakiness from CI
without touching the cause.

### Verified mechanism

Confirmed against the current code on 2026-08-19. The sequence:

1. On `SenderMessage::Complete`, the upload receive task sends the
   `awaiting_receiver` status and breaks
   ([`upload_ws.rs:451`](../../src/routes/upload_ws.rs#L451)). Breaking ends the
   task, which drops `ws_receiver`. **From this point the relay never reads that
   socket again**, even though the connection stays open for the sender's
   remaining status messages.
2. The send task continues. When the receiver confirms, it emits
   `transfer_complete`, writes `Message::Close(None)`, and breaks
   ([`upload_ws.rs:192`](../../src/routes/upload_ws.rs#L192)).
3. The client's `await_completion` returns as soon as it reads
   `transfer_complete` ([`send.rs:248`](../../cli/src/send.rs#L248)). Its
   WebSocket library replies to the relay's Close frame with a Close of its own,
   as the close handshake requires.
4. That reply lands in the receive queue of a socket nobody is reading. The
   join at [`upload_ws.rs:627`](../../src/routes/upload_ws.rs#L627) returns,
   `handle_socket` returns, and the socket is dropped with unread data queued.
   A TCP socket closed with unread data in its receive buffer sends **RST rather
   than FIN**, and the peer reports `Connection reset by peer`.

The intermittency is the race in step 3. The client already holds its success
result, so whether the user sees an error depends on whether its next socket
operation touches the connection after the reset arrives. Inserting a pause
between transfers made it disappear, which is what originally pointed at
teardown timing rather than at extraction or test logic.

### Why the obvious fix does not work

"Await the peer's Close before dropping the socket" is the right idea, but it
cannot go in the send task. `socket.split()` gives the send task the sink and
the receive task the stream; the send task has nothing to await on. The drain
has to live in the receive task, and the blocker is that the receive task exits
at step 1 — long before the Close frame in step 2 is even sent.

## Constraints and invariants

- Do not change the transfer protocol. No new messages, no new statuses.
- Preserve the one-sender, one-receiver lifecycle and every existing bound.
- A client that never replies must not be able to pin a task or a per-IP
  connection slot. Any wait needs a deadline.
- The per-IP connection release and the metrics updates must still happen after
  both halves finish.
- Do not weaken the existing failure paths: a genuine sender error, timeout, or
  cancel must still tear the session down the way it does now.

## Non-goals

- Resume or retry after a disconnect.
- Any change to the receiver socket's success path beyond confirming it does
  not have the same defect.
- Making the CLI tolerate a reset. The relay is wrong here; papering over it in
  the client would leave the browser client exposed.

## Phases

### Phase 1 — Keep the receive task alive

- [x] Replace the `break` after `awaiting_receiver`
      ([`upload_ws.rs:451`](../../src/routes/upload_ws.rs#L451)) with a
      transition into a drain state, so `ws_receiver` is not dropped.
- [x] In the drain state, keep polling the stream and discard what arrives,
      exiting on the peer's Close frame, on stream end, or on error.
- [x] Confirm no other `break` on a success path drops the stream early.

Implemented as a `sender_completed` flag plus a drain block after the loop,
rather than as a state inside it. The loop's many exit paths all mean "stop
relaying"; only the clean completion also means "stay and finish the
handshake", so the distinction reads better outside the loop.

### Phase 2 — Bound the drain

- [x] Add a named drain-deadline constant to
      [`config.rs`](../../src/config.rs) with a comment stating what it bounds.
      A second or two is enough for a close handshake on a live connection.
- [x] Apply the deadline to the drain loop, so a peer that never replies is
      abandoned rather than waited on.
- [x] Confirm the existing idle timeout does not fire during a normal drain and
      convert a clean finish into a spurious failure.

**Deviation from the plan.** A single short deadline does not work. The plan
assumed the drain only had to cover a close handshake, but the send task cannot
write `transfer_complete` until the receiver has finished writing the file,
which is unbounded work. The wait is therefore two stages: first wait for the
send task to exit, detected through the event receiver being dropped and
bounded by the existing idle timeout, then apply
`WS_CLOSE_DRAIN_TIMEOUT_SECS` to the peer's reply. Both stages are bounded, so
the property the plan wanted still holds.

### Phase 3 — Check the receiver side

- [x] Read the success path in
      [`download_ws.rs`](../../src/routes/download_ws.rs) for the same shape.
- [x] If the same defect exists there, fix it in this change; if not, note in
      the plan why it does not apply.

**Finding: the same shape exists, and it is deferred.** The initial reading in
this plan was wrong. On `ReceiverMessage::Complete` the download receive task
notifies the sender, completes the session, and breaks, dropping its half of
the socket; the send task then observes its event channel close and writes a
`Close`. The receiving client has already sent a `Close` of its own by then, so
it lands unread exactly as on the upload socket.

It is not user-visible today because the receiving client discards the result
of its close and returns success immediately, so the reset never reaches a
person. That makes it latent rather than benign.

It is deliberately not fixed here. The defect produces no observable symptom on
that socket, so a change to its teardown could not be backed by the kind of
before-and-after evidence this plan required for the upload side — which is the
same reason #30 declined to fix the upload side inside a test-stability change.
It is recorded as its own item in
[`../implementation-checklist.md`](../implementation-checklist.md).

### Phase 4 — Evidence

- [x] Run repeated small-payload CLI transfers and record the reset count.
- [x] Attempt to restore the completion-handshake assertions PR #30 removed
      from the two small-payload tests, or record why they stay out.
- [x] Run the full validation command set.

**The defect does not reproduce through separate CLI processes.** A first
harness that drove `drop send` and `drop recv` as separate processes measured
zero resets on unfixed `main` across 50 runs. The sender exits the moment it
reads `transfer_complete`, so the RST arrives at a process that no longer
touches the socket. Running that harness only against the fix would have
produced a clean result that meant nothing. It reproduces in the in-process
harness with two transfers sharing one relay, which is the shape the tests had
before #30.

Measured with the pre-#30 test file restored:

| Build | Runs | Failures |
| --- | --- | --- |
| `main`, unfixed | 30 | 3 |
| this change | 30 | 0 |
| this change | 80 | 0 |

The failure text on `main` was
`transfer should succeed: "IO error: Connection reset by peer (os error 104)"`,
matching the roughly one-in-six rate #30 reported.

**The removed assertions stay out, for a measured reason.** Restoring them
while keeping #30's per-transfer relay isolation produced zero failures on
unfixed `main` as well, across 30 runs: the isolation removes the contention
that triggers the race, so the assertions would guard nothing. The shape that
does catch it detects at roughly ten percent per run, which is too weak for CI
and would reintroduce the flakiness #30 removed. A stronger guard should be a
targeted test that drives the close handshake directly rather than a
probabilistic end-to-end race.

Note for any future evidence run: `SESSION_CREATION_LIMIT_PER_MINUTE` is 10 and
lives in process memory, so a long unbroken run against one relay starts
failing with a rate-limit error rather than anything meaningful. Restart the
relay every few transfers.

## Risks

- **A drain that waits too long** delays session cleanup and holds a per-IP
  slot. Mitigated by the Phase 2 deadline; keep it short.
- **Masking a real error.** The drain must discard only what arrives after a
  successful completion. A sender that sends garbage after `complete` is
  already outside the protocol, but the drain should not turn a socket error
  during an active transfer into a silent success.
- **Interaction with shutdown.** `SIGTERM` draining and this socket drain are
  different mechanisms with similar names. Make sure a drain deadline cannot
  outlive the shutdown transfer-wait window.
- **Test flakiness returning in a new shape.** The evidence run in Phase 4 is
  the guard; a single green run proves nothing for a one-in-six defect.

## Validation

Repeat count matters here. The historical failure rate was roughly one in six,
so a 50-run clean sweep is the minimum credible evidence and 80 runs matches
what PR #30 used to declare its fix good.

```bash
cargo test --workspace --all-targets
```

Then the repeated-transfer evidence run, recording the exact command, the
number of runs, and the number of resets observed.

Full gate before the pull request:

```bash
scripts/check-secrets.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
npm --prefix web ci
npm --prefix web run build
npm --prefix web audit --audit-level=high
```

## Acceptance criteria

- [x] At least 50 consecutive runs with zero resets: 80 runs of the in-process
      harness, against 3 failures in 30 on unfixed `main`.
- [x] No new protocol message or status.
- [x] The drain has a bounded deadline expressed as a named constant.
- [x] The receiver socket has been checked and the finding recorded.
- [x] Full validation set passes: fmt, clippy with warnings as errors, and 56
      tests.

## Kickoff prompt

```text
Read docs/plans/relay-teardown-drain-plan-2026-08-19.md and AGENTS.md.
Verify the mechanism in the Context section against the current code before
changing anything — the line references may have drifted. Then implement
Phases 1 through 4 on a topic branch. The fix belongs in the upload receive
task, not the send task; the send task owns the sink and cannot await the
peer's Close. Do not change the protocol. Record the repeated-run evidence in
this plan before opening a pull request.
```

## Open questions

- ~~How long should the drain deadline be?~~ Two seconds, applied only to the
  peer's reply once the send task has already finished. Still worth confirming
  against a slow or lossy link.
- ~~Should a drain that hits its deadline be logged at warn or debug?~~ Debug,
  on the grounds that it is benign on a healthy relay. Revisit if it turns out
  to be common enough to be worth surfacing.
- The download socket carries the same latent shape. See the Phase 3 finding.
