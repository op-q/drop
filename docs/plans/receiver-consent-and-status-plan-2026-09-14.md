# Receiver consent, cancel, and live status plan

Status: **active** — phase 1 done 2026-09-14
Created: **2026-09-14**
Last updated: **2026-09-14**

Supersedes [`receiver-confirmation-plan-2026-08-19.md`](receiver-confirmation-plan-2026-08-19.md).
That plan was written against cleartext metadata and a browser client, and the
checklist has marked it "needs revision" since encryption landed. Its three
findings (hostile display names, the file created before any prompt, and the
three clocks) all still hold and are carried forward here. Its protocol design
does not.

## Goal

Three things the user asked for on 2026-09-14, designed together because they
are one conversation:

1. **The receiver sees what is coming before anything is written**: the name it
   will be saved under, its type, its size, and for a folder how many files.
   They then accept or decline.
2. **Either side can cancel at any moment**, from a key in the interface or
   Ctrl-C at a command, and the other side is told plainly rather than finding
   out from a dropped connection.
3. **The sender sees the receiver's state throughout**: connected, entered the
   code correctly, reviewing, accepted or declined, how much the receiver has,
   finishing, done, or cancelled.

## What exists, verified against `main` at `a1e84d6`

- The receiver opens the sealed metadata before anything touches disk,
  [`recv.rs:247-265`](../../cli/src/recv.rs#L247-L265), and only then calls
  `open_target` at [`recv.rs:295`](../../cli/src/recv.rs#L295). **The consent
  point already has a natural home between those two lines.**
- `Metadata` is `{filename, mime_type, plaintext_size}`,
  [`crypto/src/envelope.rs:39-47`](../../crypto/src/envelope.rs#L39-L47), and it
  is sealed. The old plan's worry that a file count "widens the cleartext
  metadata surface" is gone: anything added here is encrypted.
- The sender's progress is **already the receiver's progress**. It advances on
  acknowledgements, not on bytes written to the socket,
  [`send.rs:634-646`](../../cli/src/send.rs#L634-L646). Status item 3's
  "how much the receiver has" needs no new frame.
- On the direct path the receiver sends `meta_ok` after opening the metadata,
  [`recv.rs:272-274`](../../cli/src/recv.rs#L272-L274). Over the relay it
  cannot, because the relay parses receiver frames into a closed set,
  [`src/domain/messages.rs:26-40`](../../src/domain/messages.rs#L26-L40), and
  fails the session on anything else,
  [`download_ws.rs:322-338`](../../src/routes/download_ws.rs#L322-L338).
- **Cancel exists in one direction only.** The sender sends `cancel` on a stream
  failure, [`send.rs:370-376`](../../cli/src/send.rs#L370-L376). The relay
  answers by reporting `cancelled` to the *sender* and an `error` to the
  receiver, and **counts it as a failed transfer**,
  [`upload_ws.rs:549-565`](../../src/routes/upload_ws.rs#L549-L565) and
  [`transfer_service.rs:56-59`](../../src/services/transfer_service.rs#L56-L59).
  The receiver has no cancel frame at all. Its only way out is `error`, which
  the relay reports as "receiver could not save the file".
- **Ctrl-C tells nobody.** Both `run`s install a handler that restores the
  terminal, deletes spool files and calls `process::exit(130)`,
  [`send.rs:117-128`](../../cli/src/send.rs#L117-L128) and
  [`recv.rs:133-137`](../../cli/src/recv.rs#L133-L137). The peer learns from a
  closed socket, and over the relay that is reported as a failure.
- The receiver's name handling:
  [`recv.rs:503-535`](../../cli/src/recv.rs#L503-L535) keeps the final component
  and numbers collisions with `create_new`. Printing it goes straight to
  `eprintln!`, [`recv.rs:282-286`](../../cli/src/recv.rs#L282-L286). **A
  filename with an escape sequence can redraw the terminal today, before any
  consent prompt exists.**
- The interface (item 7) has a chooser, file browser, options and code entry.
  Its transfer screen is phase 3 of
  [`interactive-terminal-ui-plan-2026-09-10.md`](interactive-terminal-ui-plan-2026-09-10.md),
  and it has no cancel path yet (its finding 4).

## Design

### The conversation after this plan

```text
sender                                              receiver
  │ key_exchange ─────────────────────────────────────▶ │
  │ ◀───────────────────────────────────── key_exchange │
  │ meta (sealed) ────────────────────────────────────▶ │  opens metadata
  │ ◀──────────────────────── meta_ok {confirmation}    │  proves the code, both carriers
  │   sender: "The receiver entered the code. They are reviewing the transfer."
  │                                                     │  shows preview, asks
  │ ◀──────────────────────────── accept                │  or: decline {reason}
  │ chunk, chunk, … ──────────────────────────────────▶ │
  │ ◀─────────────────────────── chunk_ack {bytes}      │  sender progress = receiver's
  │ complete ─────────────────────────────────────────▶ │
  │ ◀──────────────────── finishing                     │  large archives: extraction tail
  │ ◀──────────────────── complete {bytes_received}     │
  │
  │  at any point after connecting, either side:
  │ ◀─────────────────────────────▶ cancel {reason}
```

### New and changed peer frames

| Frame | From | Fields | Meaning |
| --- | --- | --- | --- |
| `meta_ok` | receiver | `confirmation` (hex) | **Changed**: both carriers now, with the key confirmation from [`meta-ok-key-confirmation-plan-2026-08-31.md`](meta-ok-key-confirmation-plan-2026-08-31.md) |
| `accept` | receiver | — | **New**. The sender may stream |
| `decline` | receiver | `reason`: `"declined"` or `"timed_out"` | **New**. Terminal, and not a failure |
| `cancel` | either | `reason`: `"user"`, `"write_failed"`, `"integrity"` or `"too_large"` | **Changed**: receivers may send it, and it carries a reason |
| `finishing` | receiver | — | **New**, optional. All bytes are in, and the receiver is closing the file or finishing extraction |

**Reasons are enumerations, not text.** A peer's message would land on the other
terminal verbatim, which is the escape-sequence problem again in the other
direction. Each side maps a reason to its own sentence, and an unknown reason
reads as `"user"`.

`meta_ok` on both carriers is what lets the sender say "the receiver entered the
code correctly" over the relay too, and it is the frame the key confirmation
already needs. `peers_enforce_one_guess()` keeps its job, **which carrier's
failed checkpoint consumes an attempt and may be retried**. It no longer
decides whether `meta_ok` is sent. Its doc comment, and the sender test
`the_relay_path_is_not_asked_to_pass_a_checkpoint`, change with it. Over the
relay a failed confirmation ends the transfer: the relay has already burned the
session, so there is nothing to retry.

### The relay

- `ReceiverMessage` gains `MetaOk { confirmation }`, `Accept`,
  `Decline { reason }`, `Cancel { reason }` and `Finishing`. It forwards each to
  the sender **verbatim**, by [`decisions.md`](../decisions.md) entry 12's rule
  that the peer's words are canonical. It cannot verify `confirmation` and does
  not try.
- `SenderMessage::Cancel` gains `reason` and is forwarded to the receiver as
  `cancel`, replacing today's `error: sender cancelled`.
- **Ordering the relay enforces**, which is cheap and bounds what a modified
  peer can make it do: `accept`/`decline` only after `meta` was forwarded, and
  **binary chunks refused until `accept` has been forwarded**. A sender
  that streams early is failed, and the relay budget is never spent on bytes
  nobody agreed to. The receiver checks the same thing itself (below), so this
  is defence in depth, not the defence.
- `decline` and `cancel` end the session **without** `record_transfer_failed`.
  `/metrics` gains `total_transfers_declined` and `total_transfers_cancelled`,
  which is additive to the JSON snapshot.
- Clocks. The accept deadline is 120 s, well under `SESSION_TTL_SECS` = 300 s,
  and the relay refreshes `last_activity` on every forwarded frame, so an open
  prompt never outlives the session.

### Versioning: this is a wire break, and it should fail loudly

A 0.3.0 peer against a 0.4.0 peer would otherwise hang. The new sender waits for
`accept` from an old receiver that is waiting for chunks, and both sit there
until a timeout that says nothing useful. So:

- `ENVELOPE_VERSION` goes from 1 to 2. It is already checked at both ends and at
  the relay, and a mismatch is already deliberately fatal. The receiver
  already fails with `UnsupportedVersion { found }`. Its message becomes "the
  sender is running a different version of drop (protocol 1, this is 2)".
- `DROP_ALPN` goes from `drop/transfer/1` to `drop/transfer/2`. The direct path
  then refuses at the QUIC handshake. Map the ALPN mismatch error to the same
  sentence rather than surfacing a TLS error.
- The version field now versions the conversation, not only the sealing.
  **Record that in `decisions.md`**, because entry 12 and the envelope section
  of `protocol.md` both describe it as the envelope's version.
- `meta_ok` key confirmation lands under the same bump. Both are unreleased until
  0.4.0, so one bump covers both, whichever lands first makes it. **Landed
  first, 2026-09-14:** version 2, the ALPN, `meta_ok` on both carriers with the
  relay forwarding it, and the relay's mismatch error naming both versions are
  all in (decisions entry 18). This plan's phase 2 adds the rest of the
  vocabulary under the same number.

### The receiver

**Order of operations**, replacing [`recv.rs:231-295`](../../cli/src/recv.rs#L231-L295):

1. Exchange keys, wait for `meta`, check the version, open the metadata.
2. Send `meta_ok { confirmation }`.
3. **Resolve the destination without creating it**: the sanitized name, the
   collision-numbered name it would get, and whether it is a file or an archive
   extraction. This splits `open_target` into `plan_target` (pure, reads the
   directory) and `create_target` (writes). The name shown is then the name
   used.
4. Ask. Accept sends `accept`. Decline or the deadline sends `decline` and
   returns normally, **with nothing created**.
5. `create_target`. If the name was taken between asking and creating, it is
   numbered again and the new name is printed. `create_new` makes that race
   safe, and it is rare enough not to re-ask.
6. Receive. A chunk before step 4 finished is a protocol violation: send
   `cancel { reason: "integrity" }` and stop.
7. After the last chunk, send `finishing` before `finish()`, then `complete`.

**The preview**, command surface:

```text
Incoming transfer
  Name     quarterly-report.pdf
  Type     PDF document (.pdf)
  Size     2.4 MiB
  Save as  ./quarterly-report-1.pdf   (quarterly-report.pdf already exists)
Accept? [y/N]
```

```text
Incoming transfer
  Folder   holiday-photos/   128 files, 1.2 GiB unpacked
  Size     1.1 GiB to download
  Save to  ./holiday-photos/
Accept? [y/N]
```

- **Name**: through `sanitize_for_display`, which strips C0/C1 control
  characters, DEL and bidirectional controls (U+200E, U+200F, U+061C,
  U+202A–202E, U+2066–2069) and collapses whitespace runs to one space. It caps
  at 80 columns with an ellipsis **in the middle**, so the extension stays
  visible. Every other peer-chosen name reaching a terminal goes through it too:
  the `Receiving` line, extractor warnings, and the "already exists" note.
- **Type** comes from the receiver's own reading of the final extension, not
  from the sender's `mime_type`. The sender chooses the MIME type, so it is a
  claim, while the extension of the name that will be written is a fact.
  `report.pdf` padded to `report.pdf           .exe` shows `Type  Program (.exe)`.
- **A warning line for programs and scripts**: `.exe .msi .bat .cmd .com .scr
  .ps1 .vbs .js .jar .app .dmg .pkg .sh .command .desktop .lnk`, and on
  Windows any extension in `%PATHEXT%`. It reads: "This is a program. Only open
  it if you trust the sender."
- **Folders**: `Metadata` gains optional `entry_count` and `unpacked_size`.
  Sealed, so no metadata leak. They are the sender's claims, and are shown as
  that: the expansion guard at
  [`recv.rs:55-88`](../../cli/src/recv.rs#L55-L88) still bounds what is
  actually written. After the transfer, report the **actual** file count
  alongside. `serde(default)` on both fields is not needed for compatibility,
  since the version bump already refuses old peers, but keeps the JSON tolerant.
- The existing `-f/--force` changes the Save-as line to "replacing".

**When nobody is there**: `-y`/`--yes` accepts without asking. **With no
terminal on stdin and no `--yes`, `drop recv` refuses before it connects**,
with "no terminal to ask for consent; pass --yes to accept whatever the sender
sends". Refusing before connecting matters: a refusal after key exchange would
burn a code the sender cannot reuse. (Decided by the user on 2026-09-14: require
`--yes`, no silent auto-accept.) `netlab/runner.py` passes `--yes`. So do the
Rust integration tests that drive the binary.

**Deadline**: 120 s from showing the prompt. On expiry, send `decline { reason:
"timed_out" }`, print "No answer in 2 minutes; declined.", and exit 0. The
prompt reads stdin on the blocking pool the way `AskTheTerminal` already does,
[`send.rs:455-461`](../../cli/src/send.rs#L455-L461).

### The sender

What it prints, command surface, one line per state change on stderr:

```text
Waiting for the receiver to connect...
Receiver connected.
The receiver entered the code. Waiting for them to accept...
Accepted. Sending...
Sending   45.0%  1.1 MiB / 2.4 MiB  ...        ← already acknowledgement-driven
The receiver has everything and is finishing up...
Done. The receiver confirmed all 2.4 MiB.
```

or `The receiver declined.` (exit 3), `The receiver didn't answer in time.` (exit
3), `The receiver cancelled.` (exit 4), `The receiver couldn't write the file, so
the transfer stopped.` (exit 4).

- **Waiting for `accept`**: up to 150 s, which is the receiver's 120 s plus
  margin for a slow link. Every wait loop in `send.rs` —
  `await_meta_checkpoint`, the new `await_consent`, `next_acknowledgement` and
  `await_completion` — handles `cancel` and `decline` rather than skipping
  unknown frames. Today they skip them, which is how a cancel would currently
  be ignored for the rest of a transfer.
- `--status`/`DROP_STATUS` gains a second kind of line,
  `drop-status: state=<connected|code-ok|accepted|declined|finishing|done|cancelled>`,
  so netlab and scripts can assert the conversation, not only the path. Existing
  `drop-status: path=` lines are unchanged.
- **Exit codes** become part of the program-facing surface: 0 done, 1 error,
  3 declined or timed out, 4 cancelled by the peer, 130 cancelled here. Only the
  sender distinguishes 3 and 4. A receiver that declines exits 0, because it
  did what it was asked. See open question 2.

### Cancel

One mechanism for both sides and both surfaces: a `Cancel` token (a
`tokio::sync::watch` or `CancellationToken`) passed into `send_transfer` and
`receive_transfer`. Each `transport.receive()` wait becomes a `select!` between
the frame and the token.

- **On cancel**: send `cancel { reason: "user" }` best-effort, bounded at 1 s,
  then `close()`. The receiver discards a partial single file exactly as it does
  on an integrity failure,
  [`recv.rs:442-447`](../../cli/src/recv.rs#L442-L447). A partial extraction is
  left in place and reported: "Cancelled. 37 of 128 files were already
  extracted into ./holiday-photos/." That keeps the existing reasoning about not
  deleting a tree the receiver may already have had files in.
- **Ctrl-C at a command**: the first press fires the token, and the handler
  stops calling `process::exit` directly. A **second** Ctrl-C, or the token not
  resolving within 2 s, exits immediately as today, with terminal restore and
  spool cleanup first. That keeps the "a signal must never leave the terminal
  raw" guarantee from item 7 phase 1.
- **The interface**: the review screen has `Accept` / `Decline` buttons (←/→,
  Enter; `y`/`n` shortcuts). The transfer screen, from item 7 phase 3, has a
  `Cancel` button, with Esc and `c`. Cancelling during a transfer asks "Cancel
  the transfer? The receiver will be told." (Enter confirms, Esc returns), since
  one stray Esc should not end a 4 GiB transfer at 99%.
- **Cancel-safety of reads**: `iroh` reads are not cancel-safe mid-frame. That
  is harmless here for the reason `await_meta_checkpoint` already states,
  [`send.rs:540-549`](../../cli/src/send.rs#L540-L549): after cancelling, nothing
  reads that stream again. Writing the `cancel` frame uses the send half, which
  the abandoned read does not touch.

### The sender's interface screen

Item 7 phase 3 builds the transfer screen. This plan supplies what it shows:

```text
┌ Sending quarterly-report.pdf ─────────────────────────────────┐
│  Code     7F2A91-crossover-clockwork-ridge                    │
│  Path     peer-to-peer (no Drop server)                        │
│                                                               │
│  ✓ Receiver connected                                         │
│  ✓ Code verified                                              │
│  ✓ Accepted                                                   │
│  ● Sending   ██████████░░░░░░░░░░  45%   1.1 / 2.4 MiB  8 MiB/s│
│  ○ Receiver finishing                                         │
│                                                               │
│                         [ Cancel ]                            │
└───────────────────────────────────────────────────────────────┘
```

The state list is fed by a `SenderEvent` channel that `send_transfer` emits
into. The command surface prints the same events as lines, so the two surfaces
cannot drift. This is the "sink" item 7 phase 3 already calls for, given a
concrete type.

## Phases

### Phase 0 — decisions

- [ ] `decisions.md` entry: consent before bytes, the receiver-owned type
      label, `--yes` required without a terminal, reasons as enumerations,
      version 2 versions the conversation, and exit codes 3 and 4.
- [ ] Settle open questions 1–3 below.

### Phase 1 — display safety, on its own

It fixes a live bug independent of consent: an escape sequence in a received
filename reaches the terminal today.

- [x] `cli/src/display.rs`: `for_terminal`, `name` and `peer_message`, with
      unit tests covering a CSI sequence, an OSC 8 hyperlink, an OSC window
      title, a C1 control, newlines and tabs, each bidirectional control, the
      right-to-left-override extension trick, collapsed whitespace padding, a
      middle ellipsis that keeps the extension, wide characters counted as two
      columns, and honest names (`Łódź 東京 🎉.txt`, Arabic, an emoji ZWJ
      sequence, `10:30 standup.md`) passing through unchanged.
- [x] Applied at every site where someone else's text reaches a terminal:
      - the receiver's `Receiving` line, collision note, `Saved` path, archive
        warnings and extraction line
      - every `error` frame's message, in `recv.rs`, `send.rs` and
        `transport/relay.rs`
      - the relay's HTTP error body in `client.rs`
      - the sender's own `Sending` summary and warnings
      - the interface's file browser rows
- [x] End to end through the real binary: a file named with a clear-line
      sequence and a right-to-left override crosses a real relay, and neither
      side's stderr contains `ESC` or `U+202E`. **Negative control:** with the
      `Receiving` line reverted to printing the name verbatim, the test fails
      with "an escape sequence from the name reached the receiver's terminal".
- Moved to phase 3, where the preview first needs them: `type_label(name)` and
  `is_program(name)`. Adding them here would be code with no caller.

Done 2026-09-14 on `fix/sanitize-peer-text`. 193 tests, up from 180: 12 unit
tests and one end-to-end. `unicode-width` becomes a direct dependency, and it
was already in the tree through ratatui.

### Phase 2 — protocol and relay

Bumps the version; lands with or after `meta_ok` key confirmation.

- [ ] `crypto`: `ENVELOPE_VERSION = 2`; `Metadata` gains `entry_count` and
      `unpacked_size`, both `Option<u64>`.
- [ ] `quic.rs`: `DROP_ALPN = b"drop/transfer/2"`, and the ALPN-mismatch error
      mapped to the version sentence.
- [ ] Relay: the new `ReceiverMessage` variants, forwarding, ordering
      enforcement (no chunk before `accept`), `decline`/`cancel` counted apart
      from failures, `last_activity` refreshed on forwarded frames.
- [ ] Relay tests in `tests/websocket_transfer.rs`: accept then transfer;
      decline ends the session with no failure metric; a chunk before accept
      fails the session; receiver cancel mid-stream reaches the sender as
      `cancel`; sender cancel reaches the receiver as `cancel`, not `error`.
- [ ] `protocol.md`: the conversation diagram, both frame tables, the relay's
      new ordering rules, and the version section.

### Phase 3 — receiver consent in the CLI

- [ ] Split `open_target` into `plan_target` / `create_target`.
- [ ] `ConsentPrompt` trait, the counterpart of `AnotherAttempt`, with
      `AskTheTerminal`-style and scripted implementations, so the policy is
      tested without a terminal.
- [ ] Preview rendering for file and folder; `-y/--yes`; no-terminal refusal
      **before connecting**; the 120 s deadline.
- [ ] Sender: `payload.rs` fills `entry_count` and `unpacked_size` from
      `TarPlan`; `await_consent`.
- [ ] Tests over `ScriptedTransport` and in `cli/tests/transfer.rs` over a real
      relay and over the direct pair:
      - decline leaves the destination directory **byte-for-byte unchanged**
        (listing and mtimes)
      - timeout declines
      - accept completes
      - a chunk before accept is refused
      - `--yes` never prompts
      - no terminal without `--yes` exits non-zero **without contacting the relay**
- [ ] `netlab/runner.py` passes `--yes`.

### Phase 4 — cancel and status

- [ ] The `Cancel` token through both transfer paths; every wait loop handles
      `cancel`/`decline`.
- [ ] Ctrl-C: first press cancels politely, second press or 2 s exits hard.
- [ ] `SenderEvent` channel, and the command-surface lines and
      `drop-status: state=` lines from it.
- [ ] `finishing` from the receiver.
- [ ] Exit codes 3 and 4.
- [ ] Tests:
      - receiver cancel mid-stream ends the sender with exit 4 over both carriers
      - sender cancel mid-stream leaves no partial single file on the receiver
      - a partial extraction is reported with its count
      - Ctrl-C test on Unix: send SIGINT to a child `drop send` mid-transfer and
        assert the receiver prints "The sender cancelled."
- [ ] netlab: assert the `state=` sequence in the relayed and plain-LAN
      topologies, so the lab checks the conversation as well as the carrier.

### Phase 5 — the interface

Coordinated with item 7 phase 3, which builds the transfer screen this plan
fills in.

- [ ] Receiver review screen with Accept/Decline, the same fields as the
      command preview, and the program warning.
- [ ] Sender transfer screen with the state list above and Cancel.
- [ ] Receiver transfer screen with progress and Cancel.
- [ ] Cancel confirmation during a transfer.
- [ ] Both screens show the peer's cancel or decline in words and close on a
      key, not instantly, so the person sees what happened.

### Phase 6 — documentation

- [ ] `README.md`: consent, `--yes`, cancelling, exit codes; release notes say
      scripts piping `drop recv` must add `--yes`.
- [ ] `docs/commands.md`: the preview and the status lines.
- [ ] `security.md`: display sanitisation, the receiver-owned type label, and
      the relay's new ordering enforcement.
- [ ] Mark [`receiver-confirmation-plan-2026-08-19.md`](receiver-confirmation-plan-2026-08-19.md)
      superseded, pointing here.

## Files

| File | Change |
| --- | --- |
| `crypto/src/envelope.rs` | version 2, `entry_count`, `unpacked_size` |
| `src/domain/messages.rs`, `src/routes/upload_ws.rs`, `src/routes/download_ws.rs`, `src/services/transfer_service.rs`, `src/telemetry/metrics.rs` | new receiver frames, forwarding, ordering, metrics |
| `cli/src/display.rs` | new: sanitisation, type label, program detection |
| `cli/src/recv.rs` | plan/create split, consent, cancel, `finishing` |
| `cli/src/send.rs` | `await_consent`, cancel handling in every wait, `SenderEvent`s, exit codes |
| `cli/src/payload.rs` | `entry_count`, `unpacked_size` |
| `cli/src/transport/quic.rs` | ALPN version |
| `cli/src/main.rs` | `--yes`, exit codes, Ctrl-C two-stage |
| `cli/src/ui/app.rs` | review and transfer screens |
| `cli/tests/transfer.rs`, `tests/websocket_transfer.rs`, `netlab/runner.py`, `netlab/test_transfer.py` | as phases 2–4 |
| `docs/protocol.md`, `docs/security.md`, `docs/decisions.md`, `docs/commands.md`, `README.md` | as phase 6 |

## Risks

- **Breaking scripted receivers.** Deliberate, and chosen by the user. The
  error has to name `--yes`, and the release notes have to say it first.
- **A preview that lies by omission.** The sender controls `entry_count` and
  `unpacked_size`. Labelling them as the sender's claims and reporting the
  actual counts afterwards keeps the preview honest. The expansion guard keeps
  it safe.
- **Name race between preview and creation.** Handled by renumbering, and by
  printing the name actually used. A test pins it.
- **Three clocks.** Accept deadline 120 s, sender consent wait 150 s, relay
  session TTL 300 s. In that order and pinned by a test that reads the
  constants, so a later edit that inverts them fails in CI instead of in front
  of a user.
- **Cancel racing completion.** A cancel that crosses a `complete` in flight
  must not turn a finished transfer into a reported failure. Rule: the receiver
  counts a transfer done once it has sent `complete`, and ignores a later
  `cancel`. The sender counts it done once it has received `complete`. Tested
  with a scripted transport that delivers both.
- **Two version bumps colliding.** `meta_ok` confirmation and this plan both
  change the wire. Whichever lands first bumps to 2, the other rides on it, and
  nothing is released in between.

## Validation

```bash
scripts/check-secrets.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
netlab/.venv/bin/python -m pytest netlab/
```

- [ ] Declining leaves the destination unchanged, and the sender exits 3
- [ ] A hostile filename renders inert in the preview and in every other line
- [ ] Either side's cancel reaches the other in words, over both carriers
- [ ] No terminal without `--yes` refuses before the relay is contacted
- [ ] The sender's state lines appear in order in netlab's relayed and LAN runs
- [ ] 0.3.0 against 0.4.0 fails with the version sentence in both directions, on
      both carriers

## Open questions

### Open question 1 — does the sender learn the receiver's saved name?

It would be reassuring ("saved as report-1.pdf"). It also tells the sender
something about the receiver's disk, since a numbered name means the file
already existed. **Leaning: no.** The sender learns accepted, declined and done,
and nothing about the receiver's filesystem.

### Open question 2 — exit codes

Proposed: 3 declined or timed out, 4 cancelled by the peer, 130 cancelled
locally. Is 3 for "didn't answer" right, or should a timeout be 4? **Leaning:
3.** From the sender's side both mean the receiver did not take it, and a
script retrying on 3 handles both correctly.

### Open question 3 — should the receiver be able to see the sender's name for itself?

No identity exists in the protocol. The code is the only credential. Out of
scope, and worth writing down, because "who is this from?" is the obvious next
request after a preview exists. The honest answer is the channel the code was
shared over.

## Kickoff prompt

```text
Read docs/plans/receiver-consent-and-status-plan-2026-09-14.md, docs/protocol.md,
docs/decisions.md entries 12 and 13, and AGENTS.md. Verify line references
first. Start with phase 1 (display safety) on its own branch — it fixes a live
bug and has no protocol change. Phase 2 bumps ENVELOPE_VERSION and DROP_ALPN;
coordinate with the meta_ok key confirmation plan so the wire changes once.
Nothing may be created in the receiver's destination before it accepts.
```
