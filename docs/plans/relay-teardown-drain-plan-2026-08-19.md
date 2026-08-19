# Relay teardown drain plan

Status: **proposed**
Created: **2026-08-19**
Last updated: **2026-08-19**

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

- [ ] Replace the `break` after `awaiting_receiver`
      ([`upload_ws.rs:451`](../../src/routes/upload_ws.rs#L451)) with a
      transition into a drain state, so `ws_receiver` is not dropped.
- [ ] In the drain state, keep polling the stream and discard what arrives,
      exiting on the peer's Close frame, on stream end, or on error.
- [ ] Confirm no other `break` on a success path drops the stream early.

### Phase 2 — Bound the drain

- [ ] Add a named drain-deadline constant to
      [`config.rs`](../../src/config.rs) with a comment stating what it bounds.
      A second or two is enough for a close handshake on a live connection.
- [ ] Apply the deadline to the drain loop, so a peer that never replies is
      abandoned rather than waited on.
- [ ] Confirm the existing idle timeout does not fire during a normal drain and
      convert a clean finish into a spurious failure.

### Phase 3 — Check the receiver side

- [ ] Read the success path in
      [`download_ws.rs`](../../src/routes/download_ws.rs) for the same shape.
      Initial reading suggests it is less exposed — it does not send a Close on
      success and its receive task keeps reading — but confirm rather than
      assume.
- [ ] If the same defect exists there, fix it in this change; if not, note in
      the plan why it does not apply.

### Phase 4 — Evidence

- [ ] Run repeated small-payload CLI transfers and record the reset count.
- [ ] Attempt to restore the completion-handshake assertions PR #30 removed
      from the two small-payload tests, or record why they stay out.
- [ ] Run the full validation command set.

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

- [ ] At least 50 consecutive small-payload CLI transfers with zero resets,
      with the command and counts recorded.
- [ ] No new protocol message or status.
- [ ] The drain has a bounded deadline expressed as a named constant.
- [ ] The receiver socket has been checked and the finding recorded.
- [ ] Full validation set passes.

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

- How long should the drain deadline be? A second or two should cover a close
  handshake on a live connection, but confirm against a slow or lossy link
  rather than picking a number by feel.
- Should a drain that hits its deadline be logged at warn or debug? It is
  benign on a healthy relay but a useful signal if it becomes common.
