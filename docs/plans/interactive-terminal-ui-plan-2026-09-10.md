# Interactive terminal UI plan

Status: active
Last updated: 2026-09-10 (phases 0 and 1 done, phase 2 partly)

## Goal

Make `drop send` and `drop recv` the only two things a person needs to know.
Typed bare in a terminal, each opens a small full-screen interface — a file
browser for `send`, a code field for `recv` — walks the person through the few
choices that exist, shows progress, and closes itself on both sides when the
transfer finishes or is cancelled.

Everything the CLI can do today stays reachable by flag for scripts and tests.
The interface is what a person gets; the flags are what a program gets.

## What was asked for

Recorded close to verbatim, because the phrasing carries the intent:

- The only commands are `drop send` and `drop recv`.
- `drop send` opens a file explorer inline in the terminal, minimal styling,
  "similar to nvim". Select the file or folder, continue.
- Then choose any other options, with checkmarks.
- `drop recv` gives an input for the code from the other person, then a
  destination picker, then progress.
- The transfer path auto-closes for both sides when transferred or cancelled.

**Added 2026-09-10, answering open questions 1 and 3:**

- The options screen offers *adding* a custom relay. With none added the
  transfer uses no relay, which is the default.
- Bare `drop`, no verb, prompts the person to choose send or receive.
- Context, not a decision: the browser client is old and will probably be
  removed. Nothing in this plan should be justified by browser interop, and the
  relay's remaining reason to exist here is somebody self-hosting one.

## Decisions taken before writing this

Asked and answered 2026-09-10:

1. **The flags stay, and the interface is what a TTY gets.** `drop send` with
   no path on a terminal opens the UI; `drop send FILE --transport relay`, and
   any run whose stdout is not a terminal, behave exactly as they do today.
   This follows an idiom the codebase already has (Finding 5) and leaves
   `netlab/`, `web/tests/interop.test.mjs` and `cli/tests/transfer.rs` working
   untouched — 31 flag usages across the three.
2. **ratatui 0.30.2 + crossterm 0.29.0**, crossterm with `event-stream` so key
   events select against the transfer's futures rather than blocking a thread.
   Both are pure Rust, which is the bar `cli/Cargo.toml` already sets for the
   four prebuilt targets.
3. **`--help` and `--version` survive.** They are not transfer commands and
   packaging expects them.

## Findings

These come from reading the code on `feat/netlab` at the time of writing, and
they are what shapes the phase order. Check them before building: rule 4 of the
plan contract.

### Finding 1 — the code-announce seam already exists

[`send.rs`](../../cli/src/send.rs) `SendOptions::on_code` is a
`Box<dyn FnMut(&str) + Send>`, and its doc says it exists so "presentation
stays in the binary". The UI hands in its own closure instead of the
`println!` in `printing()`. Nothing in the transfer core needs to know a UI
exists.

### Finding 2 — progress has no such seam, and will corrupt the screen

[`progress.rs`](../../cli/src/progress.rs) writes ANSI directly to stderr on a
120 ms timer. Inside an alternate screen that draws over the interface. It is
constructed in exactly two places — `recv.rs:293` and `send.rs:616` — so the
fix is contained: give `Progress` a sink, defaulting to today's stderr writer.

### Finding 3 — the sender's signal handler leaves a raw terminal raw

[`send.rs:113-121`](../../cli/src/send.rs#L113-L121) spawns a task that calls
`std::process::exit(130)` on SIGINT/SIGTERM, deliberately without unwinding, so
that the spool file is deleted by hand rather than by a destructor that will
never run. A terminal put into raw mode by the UI is never restored on that
path: the person gets a shell with no echo. **This is the worst failure mode in
the whole feature and it exists before a single screen is drawn**, which is why
terminal lifecycle is Phase 1 and screens are Phase 2.

`recv` installs no termination handler at all, so Ctrl-C there is an immediate
kill — same consequence once raw mode is in play.

### Finding 4 — the receiver cannot cancel

The sender sends `{"type":"cancel"}` when a stream fails
([`send.rs:362`](../../cli/src/send.rs#L362)) and treats an incoming
`cancelled` status as terminal (`send.rs:721`). `recv.rs` contains no cancel
path in either direction. "Auto-closes for both sides when cancelled" is
therefore half-built: sender-initiated works, receiver-initiated needs the
message sent and the sender taught to act on it. `Frame::Control(Value)` is
shared by the relay and QUIC transports, so this is one change covering both
carriers, not two.

### Finding 5 — degrade-when-not-a-terminal is an established idiom

`Progress::new` gates on `std::io::stderr().is_terminal()` and `AskTheTerminal`
on `std::io::stdin().is_terminal()`. The activation rule in Phase 0 is the same
idea applied one level up, not a new concept being introduced.

### Finding 6 — stdout is a contract

`printing()` puts the code on **stdout** and everything else on stderr,
specifically so it survives a pipe. `netlab/runner.py:352` reads the code from
the sender's stdout. The UI must not write interface bytes to stdout; ratatui
must be pointed at stderr.

### Finding 7 — the guess prompt competes for stdin

`AskTheTerminal` reads a line from stdin mid-transfer, because
[`decisions.md`](../decisions.md) entry 13 requires a human to approve each
retry after a failed code. A UI holding the terminal in raw mode cannot have a
line-reader running underneath it. The prompt has to become a screen, and it
must keep entry 13's visible attempt counter.

## Constraints and invariants

- The interface is presentation. No transfer, crypto, or path-safety logic
  moves into it. `AGENTS.md`'s extraction rules are untouched by this work.
- Entry 13's approval-per-guess, and its visible counter, survive as a screen.
- The spool file is still deleted on every exit path, including the ones that
  now also restore the terminal.
- Nothing here changes what is claimed about encryption or about which paths
  are peer-to-peer.
- No new outward network behaviour: the browser can already only meet a CLI at
  a relay, and entry 16 stands.

## Non-goals

- Mouse support. Keyboard only.
- A file manager. The browser navigates and selects; it does not rename,
  delete, or preview.
- Theming or configuration of the interface.
- Changing the wire protocol beyond the receiver-side `cancel` in Finding 4.
- Replacing the flags. They are the program-facing surface and stay documented.

## Phases

### Phase 0 — Activation rule and the non-interactive contract

- [ ] `drop send` with no positional path **and** an interactive stdin/stderr
      starts the UI. Same for `drop recv` with no code.
- [ ] Bare `drop` on a terminal opens the chooser. Bare `drop` **not** on a
      terminal keeps today's behaviour exactly: usage to stderr, exit 1. That
      asymmetry is the whole activation rule in one case, and is worth a test
      of its own.
- [ ] Any positional argument, any flag that implies non-interactive use, or a
      non-terminal stdout keeps today's behaviour exactly.
- [ ] Tests covering: piped stdout, `--status` set, an explicit path, and a
      bare invocation with a fake non-TTY.
- [ ] `netlab`, interop and `transfer.rs` suites pass unchanged. This is the
      gate for the whole plan; if it needs harness edits, decision 1 was wrong.

#### Findings

**The rule ended up stricter than "no arguments on a terminal".** All three
streams must be terminals, *including stdout*, which the interface never writes
to. Finding 6 is why: the code goes to stdout so it survives a pipe, so a
redirected stdout is somebody collecting that code and they should get it. And
`DROP_STATUS` counts even with a bare command line, because a harness exports it
once and every `drop` it spawns inherits it.

**Any flag at all means the command.** Not "any flag that implies
non-interactive use", as this plan originally said — that phrasing needs a list
of which flags those are, and the list would be wrong the first time somebody
added a flag. `drop send --compress` with no path now errors asking for a path
rather than opening the browser with a box pre-ticked. That is a real cost and
it buys a rule nobody has to look up.

### Phase 1 — Terminal lifecycle, before any screen — **done 2026-09-10**

- [x] [`ui/terminal.rs`](../../cli/src/ui/terminal.rs): a `Guard` that enters
      raw mode and the alternate screen and restores on `Drop`, plus a
      `restore()` over an `AtomicBool` that is safe to call from anywhere any
      number of times.
- [x] A panic hook that restores *before* the default hook prints, chaining to
      whatever hook was already installed. Without it a panic message goes to a
      raw-mode stderr, where every newline is a bare line feed and the text
      walks diagonally off the screen.
- [x] Finding 3's handler restores the terminal **before** deleting the spool
      file, which is the opposite order to what this plan first said. Both are
      a handful of syscalls; the one that cannot be recovered from by hand goes
      first, and a person who cannot see what they type cannot deal with
      whatever comes next either.
- [x] `recv` has a termination handler for the first time.
- [x] Checked through a pty: bare `drop`, `drop send` and `drop recv`, quit
      with `q`, `esc` and Ctrl-C. The alternate screen is entered and left on
      every one, and every exit is 0.

#### Findings

**Ctrl-C had to become a key.** In raw mode the terminal driver stops turning
it into SIGINT, so the signal-path work above does not cover the interface
itself — the key everybody reaches for would simply do nothing. `is_quit`
handles it explicitly, and the signal handler remains for the transfer that
runs after the interface has closed.

### Phase 2 — The screens — **partly done 2026-09-10**

- [x] Chooser: bare `drop` asks send or receive, and hands off to the screen
      below. It is the first screen a person ever sees, so it carries no
      settings — two choices and nothing else.
- [x] Send: file browser (navigate, select file or directory, show size), then
      an options screen. The transfer screen is phase 3; for now the interface
      closes and the transfer prints what it has always printed, which is a
      complete and usable path rather than a stub.
- [x] The options screen's relay row is an **add a custom relay** action, not a
      populated field: with nothing added there is no relay and the transfer is
      direct. A relay already named by `DROP_SERVER` shows as the current value
      rather than being hidden, because a person who exported it should be able
      to see that it is in effect.
- [x] Recv: code entry, then a destination picker defaulting to the working
      directory, with the overwrite choice on the options screen rather than
      requiring `--force`. The field is checked by `TransferCode::parse` — the
      same parser the transfer uses — on every `enter`, and refuses to advance
      until it passes.
- [ ] Both: the code shown large and copyable on the sender, and the same
      `drop recv <CODE>` line the CLI prints today. Waiting on phase 3, which
      is what puts the sender's own screen up during a transfer.

#### Findings

**`enter` had to mean "this one", not "go into this".** The browser first
shipped with `enter` opening a folder and `s` selecting anything, with both in
the hint line. The first person to use it tried to send a folder, pressed
`enter`, and was taken inside it — the report was "i cant select a folder, it
just goes into that folder instead". Two keys competing for the gesture
everybody makes first is a design error and not a discoverability one, so
`enter` now picks whatever is highlighted and `→` opens a folder, which is
already the direction key on the row above it. `s` is gone.

**Choosing the folder you are standing in is a row, not a key.** It is the
first line of the listing, `.   (this folder)`, so it needs no special
gesture and is visible rather than remembered.

**A field that is not checked where it is typed is not checked at all.** The
code field first took any non-empty string, leaving `TransferCode::parse` to
reject it — which it did, correctly, two screens later and after the interface
had closed, as a line of terminal output. The first person to try it typed
`test`, walked the rest of the flow, and got `a transfer code ends with 3 words
separated by dashes; this one has 0` from a program that had already exited.
`ask` now takes a validator, runs it on `enter`, and shows the same message
under the field.

**Going back had to exist before that change was safe.** With `enter`
selecting, selecting the wrong thing is one keystroke away, and `esc` used to
mean "give up entirely" — so the mistake was unrecoverable without restarting
the program. `Leave` now distinguishes `Quit` from `Back`, and `run` became a
loop whose next screen is whichever answer is still missing; stepping back
clears the most recent one. Settings already toggled survive it.

### Phase 3 — Progress and closing

- [ ] Give `Progress` a sink (Finding 2) and route it to the transfer screen.
- [ ] Carrier reporting (`Path    peer-to-peer (no Drop server)`) becomes a
      line on that screen; `--status` output still goes to stderr untouched.
- [ ] Both sides close themselves on completion.
- [ ] Receiver-side cancel (Finding 4): send `cancel`, and teach the sender to
      end cleanly on it.
- [ ] Esc during a transfer cancels and closes both sides.

### Phase 4 — The guess prompt as a screen

- [ ] Entry 13's approval becomes a modal screen with the attempt counter
      intact.
- [ ] Unattended behaviour unchanged: not a terminal still means one attempt.

### Phase 5 — Docs and help

- [ ] Rewrite `USAGE` so the two verbs lead and the flags read as the
      program-facing surface they now are.
- [ ] `docs/commands.md` gains an interface walkthrough; the flag reference
      stays for scripts.
- [ ] A `decisions.md` entry for "the human surface is two verbs, the flags are
      for programs", once Phase 0's activation rule has survived contact.

## Risks

- **A wrecked terminal.** Finding 3. Mitigated by ordering Phase 1 first and by
  restoring in the signal path, not only in `Drop`.
- **Two dependencies in a manifest that justifies each one.** ratatui and
  crossterm are large relative to what is there. Both carry a comment, as
  every other entry does. `crossterm`'s `event-stream` feature is deliberately
  *not* enabled yet: the interface reads keys blocking because it collects
  choices and then closes, and phase 3 is what needs a stream to select
  against a running transfer.
- **Binary size across four prebuilt targets.** ~~Measure before and after~~
  **Measured 2026-09-10: 26,988,848 → 27,482,144 bytes, +493 KB (+1.8%)**, on
  `x86_64-unknown-linux-gnu` release. Accepted; the crossterm-only fallback is
  not needed.
- **The activation rule misfiring in CI.** A harness that accidentally gets a
  TTY would hang forever on a UI nobody can drive. Phase 0's tests exist for
  this, and a timeout in the lab is the backstop.
- **Scope.** A file browser invites features. The non-goals list is the answer.

## Validation

- `cargo test --workspace`, `cargo clippy --workspace --all-targets`,
  `cargo fmt --all --check`.
- `netlab/` suite unchanged and passing — the strongest evidence that the
  program-facing surface did not move. **Run 2026-09-10: 7 passed, 1 failed**,
  the failure being `test_a_full_cone_nat_is_punched_through`, which is not
  this work. See "The netlab result" below.
- `web/tests/interop.test.mjs` unchanged and passing.
- Manual: Ctrl-C at each screen; a piped `drop send x | head -1`; a real
  transfer between two terminals on this machine, which the direct path is
  known to complete here.
- Binary size recorded before and after.

**How the interface itself is checked.** It cannot be driven from a shell, so
`netlab/`'s approach applies: spawn the binary under a pty, write keystrokes,
assert on what came back. Two things that cost time the first time and are
worth writing down — a pty from `pty.fork()` has **no window size**, so ratatui
renders into a 0×0 buffer and draws literally nothing until `TIOCSWINSZ` is
set; and ratatui redraws only changed cells, so the raw stream is a sequence of
cursor jumps and deltas rather than screens, and asserting on it means
stripping escapes and looking for words, not lines.

## The netlab result, 2026-09-10

The lab was run to satisfy phase 0's gate and came back **7 passed, 1 failed**.
The failure is `test_a_full_cone_nat_is_punched_through`: the sender reports
`a peer failed to complete the handshake: ... authentication failed` and the
receiver times out dialling it. Recorded here rather than fixed, because it
belongs to the network lab's phase 4 rather than to this plan.

Why it is not this work, in the order the evidence was gathered:

1. **The other two direct-path topologies pass.** Plain LAN transfers with no
   Drop process anywhere, and symmetric NAT falls back onto the iroh relay and
   completes. The QUIC path, the envelope and the rendezvous all work; only the
   full-cone punch does not. Nothing in this plan is topology-aware.
2. **Both processes exit 1 from transfer errors, not from signals.** The one
   behavioural change this work makes to a flag-driven run is `recv`'s new
   termination handler, which only fires on SIGINT or SIGTERM and would show as
   exit 130.
3. **It reproduces in isolation**, three runs out of three, so it is not a
   flake in a long suite.
4. **There is no baseline to compare against.** `netlab/test_transfer.py` does
   not exist at `HEAD` — it is 238 uncommitted lines — and the lab depends on
   the equally uncommitted `DROP_RENDEZVOUS_*` support in `direct.rs`. A `HEAD`
   build cannot run this lab at all, so "did it pass before" has no answer.

Point 4 is the honest limit: the gate is **satisfied for seven tests and
inconclusive for the eighth**, rather than met in full. Nothing here should be
read as proving the punch worked before.

## Open questions

1. **Does the options screen expose `--server` at all?** — **answered
   2026-09-10: yes, as an action rather than a field.** The screen offers
   adding a custom relay; adding nothing means no relay, which is the default
   and the ordinary case under entry 16. The earlier worry — putting an empty
   URL field in front of somebody who has no relay — is met by making it a
   thing you choose to do rather than a blank waiting to be filled.
2. **Still open. What does the file browser show for a directory that is
   very large?** Size is
   useful and computing it eagerly is a stat storm. Possibly lazy, possibly
   only on selection.
3. **Should `drop` bare — no verb — open a chooser?** — **answered
   2026-09-10: yes, on a terminal.** Off a terminal it still prints usage and
   exits 1; a script that runs `drop` with no arguments has made a mistake and
   should keep hearing about it.
