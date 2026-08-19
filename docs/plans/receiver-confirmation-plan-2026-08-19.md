# Receiver confirmation plan

Status: **proposed**
Created: **2026-08-19**
Last updated: **2026-08-19**

## Goal

The receiver sees what it is about to accept — name, size, type, and where it
will land — and answers yes or no before a single byte moves.

Today a receiver learns what it is getting only as it arrives. The CLI prints
`Receiving project.tar (412.7 MiB)` and immediately starts writing.

## Context

The relay already delivers everything the prompt needs, and both clients already
wait for a go-ahead signal. This is less a new handshake than moving the start
trigger and adding a decision point.

### What already exists

- Both clients begin streaming when they see the `receiver_connected` status:
  [`send.rs:141`](../../cli/src/send.rs#L141) and
  [`App.svelte:394`](../../web/src/App.svelte#L394).
- The receiver already blocks until metadata arrives before touching bytes:
  `wait_for_meta` at [`recv.rs:186`](../../cli/src/recv.rs#L186), and the web
  client's `meta` handler at
  [`App.svelte:579`](../../web/src/App.svelte#L579).
- `SenderEvent::Status` is a `&'static str`
  ([`session.rs:22`](../../src/domain/session.rs#L22)), so a new status costs
  one variant.
- `Meta` carries `filename`, `file_size`, and `mime_type`
  ([`messages.rs:6-10`](../../src/domain/messages.rs#L6-L10)).

### Three things the code makes easy to get wrong

**The filename is attacker-controlled display input.** The sender chooses it,
and [`recv.rs:189`](../../cli/src/recv.rs#L189) passes it straight to
`eprintln!` today. A name containing ANSI escape sequences can redraw the
prompt so the file the user agrees to is not the file they see; a
right-to-left override does a quieter version of the same thing. A consent
prompt is precisely where that attack pays off — it is the one place where
misleading the reader changes what they authorize. The web client escapes by
default; a terminal does not.

**The destination file is created before any prompt could run.** `open_target`
at [`recv.rs:117`](../../cli/src/recv.rs#L117) runs as soon as metadata
arrives, and it also performs the exists-check and honors `--force`. A prompt
placed after it would leave a zero-byte file behind on decline and would run
the overwrite logic before the user agreed to anything.

**A human pause outlives the session, not the socket.** The socket survives
fine: the 45-second idle timeout at
[`download_ws.rs:286`](../../src/routes/download_ws.rs#L286) is reset by the
pong answering each 15-second heartbeat ping. But `SESSION_TTL_SECS` is five
minutes ([`config.rs:16`](../../src/config.rs#L16)), so an unanswered prompt is
eventually reaped and surfaces as a confusing session expiry rather than a
timeout — while holding a session slot and a per-IP connection the whole time.

## Constraints and invariants

- Preserve one sender and one receiver per session.
- Decline is a normal outcome, not a failure. It must not increment the
  transfer-failure metric or log as an error.
- No file may be created in the destination before the receiver accepts.
- The relay must not gain the ability to accept on the receiver's behalf.
- Update [`../protocol.md`](../protocol.md) in the same change as the wire
  change.

## Non-goals

- Previewing archive contents entry by entry. The prompt describes the payload,
  not its manifest; the extractor's safety rules remain the defense against
  hostile paths.
- Letting the receiver negotiate anything other than yes or no.
- Any encryption work. This plan leaves metadata in cleartext; see
  [`end-to-end-encryption-plan-2026-08-19.md`](end-to-end-encryption-plan-2026-08-19.md).

## Phases

### Phase 1 — Protocol

- [ ] Add `Accept` and `Decline` to `ReceiverMessage`
      ([`messages.rs`](../../src/domain/messages.rs)).
- [ ] Add a `receiver_accepted` sender status; stop treating
      `receiver_connected` as the start trigger.
- [ ] Make decline terminal and distinct from cancel and error: the sender is
      told the receiver declined, the relay tears the session down as a normal
      ending, and no failure metric is incremented.
- [ ] Give the accept decision its own deadline, shorter than
      `SESSION_TTL_SECS`, and report a timeout as a decline.
- [ ] Update [`../protocol.md`](../protocol.md).

### Phase 2 — Safe rendering

- [ ] Add a display-sanitizing helper for peer-supplied names: strip control
      characters and escape sequences, neutralize bidirectional overrides, and
      cap the rendered length so a long name cannot scroll the question off
      screen.
- [ ] Apply it everywhere a peer-chosen name reaches the terminal, not only in
      the new prompt — the existing `Receiving <name>` line and the extractor's
      warnings have the same exposure.
- [ ] Unit-test it against control characters, a CSI sequence, an RTL override,
      and an overlong name.

### Phase 3 — CLI

- [ ] Move the prompt ahead of `open_target`, so declining writes nothing and
      the exists/`--force` logic runs only after acceptance.
- [ ] Render the preview: sanitized name, formatted size, type, and the
      resolved destination. For an archive, say what will happen — extraction
      into a named directory — because that is where the real risk sits.
- [ ] Add `-y`/`--yes`. Prompt when stdin is a TTY; require `--yes` when it is
      not, rather than silently auto-accepting.
- [ ] Update the README's CLI option table and operational limits.

### Phase 4 — Web

- [ ] Add the confirmation step to the receiver flow, between the `meta`
      handler and the first write.
- [ ] Ensure declining leaves no partial download and releases the session.

### Phase 5 — Optional metadata

- [ ] Consider adding a file count to `Meta` so a folder transfer can preview
      "128 files into the current directory". Decide explicitly; it widens the
      cleartext metadata surface that the encryption plan later has to move.

## Risks

- **Breaking scripted use.** Requiring `--yes` in a non-TTY is a breaking
  change for anyone piping `drop recv`. The README advertises pipeability, so
  this must be called out in operational limits and release notes. The
  alternative — auto-accepting when not a TTY — preserves compatibility but
  removes consent exactly where a human is not watching. Pre-release is the
  right time to take the stricter option.
- **Sanitization that is too aggressive** mangles legitimate non-ASCII
  filenames. Strip control and formatting characters, not everything outside
  ASCII.
- **A decline path that reuses the error path** would report a normal refusal
  as a transfer failure and pollute the metrics. Worth a dedicated test.
- **Deadline interaction.** The accept deadline, the socket idle timeout, and
  the session TTL are three different clocks. Order them deliberately:
  accept deadline < session TTL.
- **Inheriting the teardown defect.** Decline creates a new terminal close on
  the sender socket. If
  [`relay-teardown-drain-plan-2026-08-19.md`](relay-teardown-drain-plan-2026-08-19.md)
  has not landed, this path will show the same spurious reset.

## Validation

- [ ] Declining leaves no file in the destination directory.
- [ ] Declining reports a decline to the sender, not an error, and does not
      count as a failed transfer.
- [ ] A filename with control characters, a CSI sequence, and an RTL override
      renders inert.
- [ ] An unanswered prompt times out as a decline and releases the session and
      the connection slot.
- [ ] `--yes` accepts without prompting; a non-TTY without `--yes` exits with a
      clear message and a non-zero status.
- [ ] Manual: browser-to-CLI and CLI-to-browser both prompt and both honor a
      decline.
- [ ] Full validation command set passes.

## Kickoff prompt

```text
Read docs/plans/receiver-confirmation-plan-2026-08-19.md, docs/protocol.md,
and AGENTS.md. Verify the file and line references before relying on them.
Implement Phases 1 through 4 on a topic branch; treat Phase 5 as an explicit
decision to record, not an assumption. The prompt must run before open_target
in cli/src/recv.rs, and every peer-supplied name reaching a terminal must be
sanitized first. Decline is a normal outcome and must not count as a transfer
failure. Update docs/protocol.md and the README in the same change.
```

## Open questions

- How long should the accept deadline be? Long enough to read a prompt and
  think, short enough that an abandoned prompt does not hold a slot. Somewhere
  under two minutes, and well under the five-minute session TTL.
- Should the browser show the destination? It cannot know it in the same way
  the CLI does, so the two previews will not be identical. Decide what the web
  prompt claims rather than implying a path it does not control.
- Does the sender need to distinguish "declined" from "timed out"? One status
  is simpler; two are more honest.
