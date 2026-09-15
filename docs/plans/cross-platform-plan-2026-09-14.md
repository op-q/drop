# Cross-platform plan: Windows, macOS and Linux, and transfers between them

Status: **active** — phases 0–4 written (2–4 on 2026-09-15); phase 4's first CI run and phase 5, the manual Windows checklist, next
Created: **2026-09-14**
Last updated: **2026-09-15**

## Goal

`drop` installs and runs on Windows, macOS and Linux, and a transfer between any
two of them — a file or a folder, over either path — arrives intact, or tells
the receiver exactly what it could not reproduce and why.

The user set this as very important on 2026-09-14. At the time of writing
the honest description is: **Linux works, macOS builds and has never had a test
run on it, and Windows has no build at all.**

## Where things stand, verified 2026-09-14

| Platform | Built at release | Tests run in CI | Installer | Notes |
| --- | --- | --- | --- | --- |
| Linux x86_64 / aarch64 (musl) | yes | yes, Ubuntu only | `install.sh` | the only platform anything has been proven on |
| macOS x86_64 / aarch64 | yes | **no** | `install.sh` | release only runs `drop --version` on it |
| Windows | **no** | **no** | **none** | `install.sh` stops with "unsupported operating system" |

- [`release.yml`](../../.github/workflows/release.yml) builds four targets, all
  Linux or macOS. Its "Verify the binary runs" step is the only thing that has
  ever executed a macOS binary, and all it runs is `--version`.
- [`ci.yml`](../../.github/workflows/ci.yml)'s `rust` job runs on
  `ubuntu-24.04` alone. Every `#[cfg(not(unix))]` branch in the CLI has
  therefore **never been compiled**, let alone run or linted.
- `web/public/install.sh` detects the platform with `uname -s` and supports
  `Linux` and `Darwin` only. Phase 0 of the browser removal plan moves it to
  `scripts/install.sh`.

The code is not Unix-only by accident. Someone thought about Windows: there are
`cfg(not(unix))` fallbacks for file modes, spool file permissions, signals and
symlinks, and the interface already drops key-release events because "Windows
reports both edges of a key" ([`ui/app.rs:146-158`](../../cli/src/ui/app.rs#L146-L158)).
What is missing is anything that proves those branches work, plus a handful of
places where Windows behaves differently in a way nobody has accounted for.

## Findings

Each one is from reading the source, not from running on Windows, which nobody
can do yet. Phase 0 exists to turn them into observed behaviour.

### Finding 1 — a symlink in a folder breaks every Linux-to-Windows folder transfer

[`untar.rs:417-423`](../../cli/src/untar.rs#L417-L423) makes `create_symlink`
return `Unsupported` on anything that is not Unix, and
[`untar.rs:273`](../../cli/src/untar.rs#L273) propagates that error with `?`.
The whole extraction stops at the first symlink, mid-transfer, and leaves a
partial tree behind. Symlinks are common in real folders: `node_modules/.bin`,
Python virtual environments, most build output. So in practice **most folders
sent from Linux or macOS to Windows fail**.

The fix is small: a symlink the receiver cannot create becomes a warning, the
same way an existing file already does at
[`untar.rs:266-269`](../../cli/src/untar.rs#L266-L269). Creating links on Windows
needs Developer Mode or an elevated process, so trying and falling back is worse
than not trying.

### Finding 2 — names a peer chooses mean different things to Windows

The extractor judges a path safe with `Path::components`
([`tar.rs:468-497`](../../cli/src/tar.rs#L468-L497)) and then checks it against
the filesystem. Both checks are sound on Unix. On Windows a *normal* path
component can still be interpreted by the filesystem API:

| Name in the archive | What Windows does with it | Consequence |
| --- | --- | --- |
| `notes.txt:hidden` | NTFS **alternate data stream** `hidden` on `notes.txt` | Data attached invisibly to a file that may already exist and belong to the receiver |
| `notes.txt::$DATA` | the *main* stream of `notes.txt` | Can reach an existing file under a name the exists-check at [`untar.rs:279`](../../cli/src/untar.rs#L279) does not match on text, so the no-replace rule's protection is unproven |
| `CON`, `NUL`, `AUX`, `PRN`, `COM1`–`COM9`, `LPT1`–`LPT9`, including `nul.txt` and any case | a **device**, not a file | Content written to a device, or a creation error that aborts the extraction |
| `report.` or `report ` (trailing dot or space) | silently stripped, so it becomes `report` | The name validated is not the name written |
| `<>"|?*` or a control character | an invalid name | Creating it fails, and `?` aborts the extraction |

The single-file path has the same exposure one level up:
[`recv.rs:503-507`](../../cli/src/recv.rs#L503-L507) keeps the final component of
a name the sender chose. `report:v2.pdf` from a Linux sender creates a stream,
not a file.

Not all of this is hostile. Colons are ordinary on Linux and macOS, and a folder
full of `10:30 standup.md` is exactly what an honest user sends. So the rule
cannot be "refuse". See [open question 1](#open-question-1--rewrite-or-refuse-a-name-windows-cannot-store).

The same failure also happens on Linux and macOS when a receiver extracts onto
an exFAT or NTFS USB drive. That case needs no rewriting, only for an
uncreatable entry to become a warning rather than an abort.

### Finding 3 — a Windows sender puts Windows syntax in symlink targets

[`tar.rs:146`](../../cli/src/tar.rs#L146) records `fs::read_link` output
verbatim. On Windows that is `..\shared\config` or `C:\Users\...`. A Linux
receiver sees one normal component containing backslashes, finds it inside the
destination, and creates a link that never resolves. It is harmless, since the
target stays inside the destination by construction, but it is wrong. Relative
targets should be written with `/`. An absolute one should be skipped with a
warning, as it would dangle on any other machine.

### Finding 4 — the progress line assumes a VT terminal

[`progress.rs:88`](../../cli/src/progress.rs#L88) writes `\r\x1b[2K`. Windows
Terminal handles that. The older console that `cmd.exe` and PowerShell 5 still
open on many machines handles it only after a program turns on VT processing,
and nothing does. Without it the screen shows `←[2K` before every update.
crossterm already carries the Windows code to turn VT processing on, and the
interface depends on crossterm.

### Finding 5 — closing the window leaves the user's bytes in `%TEMP%`

[`payload.rs:85-88`](../../cli/src/payload.rs#L85-L88) waits only for Ctrl-C on
non-Unix. Closing the console window, logging off and shutting down send
`CTRL_CLOSE_EVENT`, `CTRL_LOGOFF_EVENT` and `CTRL_SHUTDOWN_EVENT`, which nobody
handles. The process dies and a compressed send's spool file survives. That file
is a copy of the user's data in a temporary directory, which is exactly what the
spool cleanup exists to prevent. `tokio::signal::windows` exposes all three.
Windows allows a few seconds after a close event, which is enough to delete one
file.

### Finding 6 — executable bits do not survive a Windows sender

[`tar.rs:207-214`](../../cli/src/tar.rs#L207-L214) records `0o644`/`0o755` from
the read-only flag, because Windows has no executable bit. A script sent from
Windows to Linux arrives non-executable. Nothing can recover a bit that was
never recorded, so this is a documented limitation, not a bug.

### Finding 7 — tests that can only fail on the platform they were written on

Seven tests in [`cli/tests/archive.rs`](../../cli/tests/archive.rs) and one in
[`cli/tests/transfer.rs:366`](../../cli/tests/transfer.rs#L366) are
`#[cfg(unix)]`. That is correct, since they plant symlinks. But Windows has no
equivalent tests for the hostile names in finding 2. Those threats exist only
there, so today they are covered nowhere.

### Finding 8 — Windows-only behaviour outside the code

- **Firewall.** The first time `drop.exe` binds a UDP socket, Windows Defender
  Firewall asks whether to allow it on private and public networks. Answering
  Cancel blocks unsolicited inbound traffic. Outbound still works, so a
  direct transfer usually still succeeds, sometimes only through iroh's relay.
  The dialog appears in the middle of a transfer and has to be documented or it
  reads as a bug.
- **SmartScreen.** An unsigned `.exe` downloaded through a browser gets "Windows
  protected your PC". A binary fetched with PowerShell's `Invoke-WebRequest`
  carries no Mark of the Web and does not. So the installer route avoids it and
  a manual download does not. Signing is out of scope, see
  [open question 3](#open-question-3--code-signing).
- **C runtime.** A default `*-pc-windows-msvc` build links the Visual C++
  runtime dynamically, and a clean machine without the redistributable refuses
  to start it. `-C target-feature=+crt-static` removes the dependency, the same
  reason the Linux targets are musl.

### What does *not* differ, and why no cross-OS protocol work is needed

The wire is platform-neutral by construction. Control frames are JSON, the
envelope's integers are big-endian ([`protocol.md`](../protocol.md), *Sealing*),
chunks are opaque bytes, and nothing on the wire is a path. A Windows peer and a
Linux peer run the same pure-Rust envelope. **Everything platform-specific is at
the two ends: how a sender reads a tree, and how a receiver writes one.** That is
where this plan spends its effort.

## Constraints

- The archive invariants in [AGENTS.md](../../AGENTS.md) do not relax on any
  platform. Absolute paths, `..`, drive and UNC prefixes and anything that
  traverses a link are still refused. Any rewriting in phase 1 happens to one
  normal component and must never introduce a separator.
- Nothing already on the receiver's disk is replaced unless `--force` was
  given, on every platform.
- No C toolchain dependencies. The existing bar in
  [`cli/Cargo.toml`](../../cli/Cargo.toml) stays: pure Rust, so every target
  builds on its own native runner.
- The wire does not change. A Windows build interoperates with a Linux build
  of the same version and nothing more is promised.

## Phases

### Phase 0 — tests on all three operating systems

Nothing below can be verified until this exists, so it comes first and its
failures are the input to the rest.

- [x] `ci.yml`: a `rust-platforms` job running Clippy and the tests on
      `macos-14` and `windows-2025`. It is a separate job, not a matrix on
      `rust`, because branch protection requires a check named exactly "Rust"
      and a matrix would rename it. Formatting stays on Linux only. The test
      step runs even when Clippy fails, so one run reports both.
- [x] Record what fails on the first run in this plan, dated, before fixing
      any of it. See below.
- [x] Make the suite pass on all three without skipping anything that is not
      genuinely inapplicable. Each new `cfg` has a comment saying why.
- [x] Line endings: no fixture failed on Windows, so `.gitattributes`' `eol=lf`
      holds for what the tests read. Confirmed by the Windows run, not assumed.
- [ ] Add `Rust (macos-14)` and `Rust (windows-2025)` to branch protection's
      required checks. A repository setting, for the owner.

#### What the runners found, 2026-09-14

**Run 1.** macOS: Clippy and all tests green on the first attempt. Windows:
Clippy failed on two items, and nothing else. `cli/tests/archive.rs` imported
`traverses_only_real_dirs`, and `cli/tests/transfer.rs` defined `archive_of`.
Both are used only by `#[cfg(unix)]` symlink tests. **Every `cfg(not(unix))`
branch in the CLI compiled and passed Clippy**, the first time any of it had
been compiled. Fixed by gating the two items. The test step had not run,
because it followed Clippy.

**Run 2.** Windows: Clippy and **the whole suite green**, about 6 minutes on a
cold runner. macOS: one failure,
`upload_socket_rejects_chunks_over_the_message_cap`, with
`expected oversized chunk to be written: Io(... BrokenPipe ...)`. It passed on
run 1, so it is a race. The relay refuses an oversized frame from its header
and closes the socket, possibly while the test is still writing the frame body.
Linux's socket buffer usually absorbs the write, and macOS's usually does not.
The relay's behaviour was correct both times; the test demanded that its own
doomed write succeed. The test now accepts a broken pipe, reset or closed
connection on that write and still asserts what matters: the receiver gets an
error and never a byte of the chunk.

**What green does not mean.** Windows passing the suite says the suite has
nothing Windows-specific in it. It does not say Windows works. Findings 1–5
above have no tests at all: the seven symlink tests are Unix-only, and nothing
exercises reserved names, alternate data streams, trailing dots, or a symlink
arriving at a Windows receiver. Phase 1 writes those tests, and they are
expected to fail before its fixes.

**Gate:** met, pending the final run with the race fixed.

### Phase 1 — a Windows receiver writes what it can and reports the rest

- [x] Finding 1: a symlink the platform cannot create becomes a warning,
      `skipped symlink {name}: this system cannot create symbolic links`, and
      extraction continues.
- [x] Finding 2, archives: a pure function that takes one normal component and
      returns what Windows can store, compiled and unit-tested on **every**
      platform and applied only when the receiver runs on Windows. Rewrite
      `: < > " | ? *` and control characters to `_`, a trailing dot or space
      to `_`, and a reserved device name to the same name with `_` after its
      stem (`CON` → `CON_`, `nul.txt` → `nul_.txt`). A rewritten name produces a
      warning naming both forms. Existence and link checks run on the
      **rewritten** path, so the no-replace rule and the link checks judge the
      path that is actually written.
- [x] Finding 2, single files: the same function applied to the name in
      [`recv.rs:503-507`](../../cli/src/recv.rs#L503-L507), before collision
      numbering, so `report:v2.pdf` saves as `report_v2.pdf` and the preview in
      the consent plan shows that name.
- [x] Every platform: an entry whose creation fails with an invalid-name or
      permission error becomes a warning, not an abort, so a Linux receiver
      writing to an exFAT drive gets every file it can store. Disk-full and other
      I/O errors still abort, because continuing would only fail again.
- [x] Case-insensitive collisions (`Makefile` and `makefile` on Windows or
      macOS): no code change is expected, since the second is "already exists"
      and is skipped. Pin it with a test that runs on the two platforms where it
      applies.
- [x] Windows-only extraction tests, the counterpart of the Unix symlink tests,
      covering `a:b`, `a::$DATA` next to an existing `a`, `CON`, `nul.txt`,
      `report.`, `report `, `a\..\b`, `C:x`, `\\?\C:\x` and `\\server\share\x`. Each
      asserts both what was written and that nothing outside the destination or
      in an existing file changed.

Done 2026-09-14 on `fix/windows-receiver`. Implementation notes, where they
differ from the list above:

- The policy lives in `cli/src/names.rs`: `Naming::{AsSent, Windows}`,
  `windows_component`, and `received_file_name`. It is compiled and
  unit-tested on every platform, and `TarExtractor::naming` lets the portable
  tests drive the Windows rewriting through the real extractor on Linux.
- **Archive paths are now split on `/` by hand** rather than handed to `Path`,
  which reads `C:x` as a drive on Windows and as a name elsewhere. Every
  platform now judges the same components. A backslash inside a component is
  refused everywhere, as before on Unix and newly spelled out for Windows.
- `COM¹`–`COM³`, `LPT¹`–`LPT³`, `CONIN$` and `CONOUT$` are also reserved
  device names, and are covered.
- Trailing dots and spaces are replaced one for one (`report..` becomes
  `report__`), not collapsed, so two different names stay different.
- "Warn and continue" covers `InvalidFilename` and `InvalidInput` only. A
  permission error, a full disk or an I/O error still ends extraction, because
  it would fail again on the next entry. Symlinks are the exception: on Windows
  they are not attempted at all (with `--force`, attempting would delete the
  file already at that path first). On Unix, a filesystem that cannot store a
  link, such as exFAT, answers with a permission error, which is skipped.
- Pinned without Windows by a 300-byte component, which every mainstream
  filesystem refuses. **Negative control:** with the old behaviour restored,
  the test fails with extraction aborting at that entry.
- Windows-only and macOS/Windows-only tests are written and run in CI. They
  have not run on a real Windows machine; phase 5 does that.

**Gate:** a hostile archive built from every name above extracts on a Windows
runner without writing outside the destination or into an existing file, and
an honest archive from a Linux tree with symlinks and colons extracts with
warnings and every representable file intact.

### Phase 2 — a Windows sender produces a portable archive, and cleans up

- [x] Finding 3: on Windows, write relative link targets with `/`, and skip
      absolute targets with a warning. Tested by a pure function on every
      platform.
- [x] Finding 5: `wait_for_termination` also resolves on `ctrl_close`,
      `ctrl_logoff` and `ctrl_shutdown` on Windows. Test: the existing spool
      cleanup test already runs on every platform after phase 0. Add a Windows
      test that the spool file can be deleted while the payload still holds it
      open. `std` opens files with `FILE_SHARE_DELETE`, but that is exactly the
      kind of thing to pin rather than trust.
- [x] Finding 4: turn VT processing on once at startup on Windows. If the
      console refuses, fall back to `\r` and padding without escapes.

Done 2026-09-15 on `feat/windows-sender`. Notes:

- `tar::portable_link_target` is pure and tested on every platform. A link left
  out is reported with its reason, which needed `TarPlan::skipped` to carry
  reasons: it used to hold bare paths that the payload labelled "unsupported
  file type".
- `wait_for_termination` also resolves on Ctrl-Break, the console's other
  interrupt. It now feeds the two-stage cancel from the consent plan's phase 4,
  so closing the window cancels, tells the peer if it can, and cleans up within
  the few seconds Windows allows.
- VT processing is turned on through `crossterm::ansi_support::supports_ansi`.
  Where that fails, the progress line is redrawn with `\r` and space padding
  instead of escapes. The padding is a pure function with its own tests.
- The open-spool test streams an incompressible 12-chunk payload, so the reader
  is still holding the file open when it is deleted.
- **None of the `cfg(windows)` code compiles on this machine**, which has no
  Windows target. The Windows CI runner is the first compiler to see it, and the
  manual checklist in phase 5 is the first to see it run in a real console.

### Phase 3 — build, package and install for Windows

- [x] `release.yml`: add `x86_64-pc-windows-msvc` on `windows-2025`, and
      `aarch64-pc-windows-msvc` on `windows-11-arm` (see risks). Package as
      `drop-<target>.zip` holding `drop.exe`, `LICENSE` and `README.md`, since
      `Expand-Archive` is built in and `tar.gz` is not a Windows habit. The
      checksum step covers `drop-*.zip` as well as `drop-*.tar.gz`.
- [x] Static CRT for both Windows targets through
      `[target.'cfg(all(windows, target_env = "msvc"))'] rustflags`. **Note for
      the browser removal plan's phase 2:** that phase deletes
      `.cargo/config.toml`, and must keep this section if phase 3 has landed.
- [x] The "Verify the binary runs" step works unchanged on a Windows runner
      with `shell: bash`. Confirm it, and confirm the binary does not depend on
      `VCRUNTIME140.dll` (`dumpbin /dependents`).
- [x] `scripts/install.ps1`: detect the architecture, download the zip and
      `checksums.txt`, check with `Get-FileHash -Algorithm SHA256`, install to
      `%LOCALAPPDATA%\Programs\drop\drop.exe`, and add that directory to the
      **user** `PATH` only if it is absent. Honour `DROP_VERSION`,
      `DROP_INSTALL_DIR` and `DROP_RELEASE_BASE` to match `install.sh`. Publish
      it as a release asset beside `install.sh`.
- [x] `install.sh` on `MINGW*`/`MSYS*`/`CYGWIN*`: point at the PowerShell
      installer by name rather than printing "unsupported operating system".
- [x] `README.md`: a Windows install line,
      `irm https://github.com/op-q/drop/releases/latest/download/install.ps1 | iex`,
      and a platform table that matches phase 0's CI rather than intent.

**Gate:** a release built from a `workflow_dispatch` on an existing tag's
successor publishes both zips and `install.ps1`, and `install.ps1` installs a
binary that runs `drop --version` on the Windows runner.

Done 2026-09-15 on `feat/windows-release`, as far as it can be without tagging a
release. Notes, where they differ from the list above:

- **Static CRT through `RUSTFLAGS` on the release build step, not a cargo
  config.** Without `--target`, config `rustflags` also reach the build scripts
  and proc-macros compiled for the host. With `--target`, which the release
  build uses, the environment variable reaches the target's artifacts only. The
  note to the browser removal plan about keeping `.cargo/config.toml` is moot:
  there is no config file.
- **`aarch64-pc-windows-msvc` is `continue-on-error`** (open question 2 answered
  as it leaned). If `ring` needs a toolchain the runner lacks, the release still
  publishes, without that zip.
- **Not checked:** that `drop.exe` imports no `VCRUNTIME140.dll`. `dumpbin` is
  not on the runner's bash `PATH`, and a guessed-at path to it would be the
  fragile part of the release. Phase 5's clean-machine install is where a
  missing runtime would show.
- **A new `Installer` CI job runs both installers for real** on every pull
  request. It builds the CLI, lays out a package, `checksums.txt` and a tampered
  copy the way a release does, serves them on loopback, and points
  `DROP_RELEASE_BASE` at them. It checks that the installed binary runs, and that a
  checksum mismatch is refused and installs nothing. On Windows it runs
  `install.ps1` under both `pwsh` 7 and Windows PowerShell 5. The Linux half was
  run locally, and passes. The Windows half has only CI to run it.
- `install.ps1` runs inside a script block and fails with `throw`, not `exit`.
  Run through `irm | iex`, a bare `exit` would close the user's PowerShell
  window, and preference variables set at the top level would leak into their
  session.
- An existing `drop.exe` is renamed aside before the new one is copied, so an
  upgrade works while an old `drop` is still running a transfer.

### Phase 4 — archives produced on one OS, extracted on the others, in CI

The wire is platform-neutral (see above), so an archive written on one OS and
extracted on another is the cross-OS risk that remains. It can be tested
without networking two runners together.

- [x] A separate workflow, `cross-os.yml`, triggered on pull requests that touch
      `cli/src/tar.rs`, `cli/src/untar.rs`, `cli/src/payload.rs` or
      `cli/src/recv.rs`, and nightly. Job `produce`, a matrix over three OSes,
      builds a fixed tree using every name that platform can hold: nested
      folders, an empty folder, a large file, a zero-byte file, Unicode names,
      a name longer than 100 bytes, a relative symlink where the platform
      allows one, an executable script, and colons where the platform allows
      them. It writes the archive bytes `TarPlan` produces, uncompressed and
      gzipped, and uploads them with a manifest of expected contents.
- [x] Job `consume`, needing `produce`, is also a matrix over three OSes. It
      downloads all three archives and extracts each through the real receive
      path, not only through `TarExtractor`, so the gzip and naming code runs
      too. It then checks against the manifest: identical bytes for every
      representable file, and the expected warning for every file that is not.
- [x] Driven by `#[ignore]`d tests reading `DROP_FIXTURE_OUT` /
      `DROP_FIXTURE_IN`, so the logic lives in Rust with the rest of the tests
      and the workflow is only plumbing.

Nine pairings, three of which are same-OS controls.

Considered and **rejected**: a live transfer between two CI runners. The runners
have no channel to swap a code mid-run, and making one would mean a test-only
way to fix a transfer code in advance. That is a knob that weakens the one
secret the protocol has, and it would exist in shipped binaries.

Written 2026-09-15 on `ci/cross-os-archives`; the nine pairings have their first
run on that branch's pull request. Notes:

- Fixture contents are derived from each file's path, so a consumer regenerates
  what it expects instead of the producer shipping hashes or copies.
- The tree covers: nested folders, an empty folder, an empty file, a 3 MiB file,
  Unicode names, a name too long for a ustar header, a path deeper than 260
  characters, and an executable script. Only a Unix producer adds a relative
  symlink, `10:30 standup.md`, `nul.txt` and `trailing dot.`, which a Windows
  receiver has to rewrite or skip.
- The consumer expects Windows names rewritten exactly as `names::Naming` says,
  links skipped (and nothing left in their place) on Windows, and executable bits
  kept on Unix.
- **Negative control:** with one byte flipped in the middle of the Linux tar,
  `consume` fails with `nested/deeper/data.bin: contents differ`.
- Linux to Linux was run locally, for both the tar and the gzipped tar.

### Phase 5 — a real Windows machine against a real Linux one

CI runners are clean virtual machines without the firewall prompt, antivirus
scanning or a legacy console. The user has a Windows machine and this Linux one.

- [ ] Run the checklist below. Record results in
      `docs/validation/cross-os-windows-linux-<date>.md`, with the Windows
      version, the terminal used and anything that surprised.

```text
Build      drop.exe from the phase 3 release (or a CI artifact), drop on Linux from the same commit.

Direct path (no DROP_SERVER set)
[ ] Linux -> Windows: one file, 1 GiB+          saved intact (compare SHA-256)
[ ] Windows -> Linux: one file, 1 GiB+          saved intact
[ ] Linux -> Windows: folder with symlinks, colons, CON.txt, Unicode, an empty dir
                                                warnings name each rewrite/skip; the rest intact
[ ] Windows -> Linux: folder with nested dirs, Unicode, a read-only file
                                                intact; permissions as documented
[ ] First run on Windows: note the firewall dialog, and what happens on Allow vs Cancel

Relay path (api running on the Linux box, DROP_SERVER=http://<lan-ip>:<port> on both)
[ ] Windows -> Linux and Linux -> Windows, one file each, --transport relay

Interface and lifecycle on Windows
[ ] `drop send` and `drop recv` bare in Windows Terminal: every screen, arrow keys move one row
[ ] the same in cmd.exe / conhost: no raw escape codes on screen
[ ] Ctrl-C during a send and during a receive: terminal restored, peer told, no partial file
[ ] Close the console window mid-send of a compressed folder: no drop-* spool file left in %TEMP%
[ ] install.ps1 on a machine without drop: installs, PATH updated, new shell finds `drop`
```

**Gate:** every box ticked, or its failure recorded with an item created for it.

### Phase 6 — documentation and the decision

- [ ] `README.md`: platforms, install per platform, and known limitations:
      executable bits from Windows (finding 6), symlinks on Windows (finding 1),
      rewritten names (finding 2), the firewall dialog and SmartScreen
      (finding 8).
- [ ] [`security.md`](../security.md): Windows name handling as part of the
      hostile-archive section, including why streams and device names are
      rewritten and not merely refused.
- [ ] [`decisions.md`](../decisions.md): an entry for the Windows name policy,
      once open question 1 is settled.
- [ ] [`release-checklist.md`](../release-checklist.md): the Windows assets and
      `install.ps1` in the publish check.

## Files

| File | Change |
| --- | --- |
| `.github/workflows/ci.yml` | OS matrix for the Rust job |
| `.github/workflows/cross-os.yml` | new: produce/consume archive matrix |
| `.github/workflows/release.yml` | Windows targets, zip packaging, checksums, `install.ps1` |
| `.cargo/config.toml` | static CRT for msvc targets |
| `cli/src/untar.rs` | symlink and invalid-name failures become warnings; Windows names |
| `cli/src/tar.rs` | Windows component rewrite function; portable link targets |
| `cli/src/recv.rs` | single-file names through the same function |
| `cli/src/payload.rs` | Windows console close, logoff, shutdown |
| `cli/src/progress.rs` or `cli/src/main.rs` | VT processing on Windows |
| `cli/tests/archive.rs`, `cli/tests/transfer.rs` | Windows hostile-name tests; fixture producer and consumer |
| `scripts/install.ps1`, `scripts/install.sh` | new installer; pointer from MSYS |
| `README.md`, `docs/security.md`, `docs/decisions.md`, `docs/release-checklist.md` | as phase 6 |

## Risks

- **Phase 0 finds more than the findings list.** Likely, not a failure of the
  plan. The findings come from reading; the runner is the first thing that has
  ever executed this code on Windows. Budget for it and record what it finds.
- **`aarch64-pc-windows-msvc` may not build.** iroh's TLS stack uses `ring`,
  and `ring` on Arm Windows has at times needed `clang` on the runner. If it
  fails, ship x86_64 only and say so. Windows 11 on Arm runs x86_64 binaries
  under emulation, and a missing native build is a performance note, not a
  missing platform.
- **Rewriting names is a policy and can be wrong for someone.** A receiver who
  expected `a:b` gets `a_b`. The warning is what keeps this honest, and it has
  to be in front of the user, not buried. The interface's transfer screen
  should list it.
- **Rewriting creates collisions.** `a:b` and `a_b` in the same folder map to
  one name. The no-replace rule makes the second a skip with a warning, which is
  safe. A later version could number the collision instead. Pinned by a test
  so it is a known behaviour, not an accident.
- **Windows path length.** Paths over 260 characters fail unless long paths
  are enabled on the machine. A deep folder from Linux can exceed it. Rust's
  `std` adds the `\\?\` prefix for absolute paths and avoids the limit in most
  cases. Include a deep path in the phase 4 fixture tree so this is observed
  rather than assumed.
- **Antivirus.** Real-time scanning on a real Windows machine can hold a
  just-written file open, which turns `remove_file` in the extractor's
  replace-safely step ([`untar.rs:294`](../../cli/src/untar.rs#L294)) into a
  sharing violation. It only shows on a real machine, which is one reason
  phase 5 exists.
- **CI cost.** Windows and macOS runners are slower, and on private repositories
  they bill at a multiple. The repository is public, so the minutes are free,
  but a Windows `cargo test` from cold is realistically 15-25 minutes. Use a
  dependency cache and set `timeout-minutes` from what the first run takes, not
  from the Linux job's 15.

## Validation

```bash
scripts/check-secrets.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings   # on all three runners
cargo test --workspace --all-targets                                   # on all three runners
```

- [ ] CI green on Linux, macOS and Windows (phase 0 gate)
- [ ] Hostile Windows names contained, honest ones rewritten with warnings (phase 1 gate)
- [ ] Windows zips, checksums and `install.ps1` published and installing (phase 3 gate)
- [ ] Nine producer/consumer pairings green (phase 4)
- [ ] Manual Windows ↔ Linux checklist recorded (phase 5 gate)

## Open questions

### Open question 1 — rewrite or refuse a name Windows cannot store

[`tar.rs:463-467`](../../cli/src/tar.rs#L463-L467) sets the precedent: a
dangerous path is *refused, not normalized*, "because a rewritten path silently
changes where a hostile archive lands". That argument is about paths that would
escape, where normalizing `..` away changes the directory an entry lands in. A
character rewritten inside one component changes the leaf name, never the
directory. Nothing is introduced that could be a separator, and the filesystem
checks run afterwards on the result.

Refusing would drop every file with a colon from an honest Linux folder, which
is data loss in the common case to prevent a threat the rewrite also prevents.

**Leaning: rewrite, with a warning per entry, and record it in `decisions.md`
as a deliberate, narrow exception to the refuse-don't-normalize rule.** 7-Zip
makes the same choice on Windows.

### Open question 2 — `aarch64-pc-windows-msvc` in the first Windows release, or later

Leaning: attempt it in phase 3 and drop it without ceremony if `ring` needs a
toolchain the runner lacks. See risks.

### Open question 3 — code signing

An Authenticode certificate costs money yearly and needs a secret in CI. The
installer path already avoids SmartScreen. **Leaning: not now.** Document the
SmartScreen dialog for manual downloads and revisit if people report it.

## Dependencies on other plans

- **Browser client removal, phase 0** moves `install.sh` to `scripts/`. Phase 3
  here adds `install.ps1` beside it and edits the same `release.yml` publish
  step, so it lands after phase 0.
- **Browser client removal, phase 2** deletes `.cargo/config.toml`. If phase 3
  here has already landed, keep the Windows section.
- **Receiver consent and status** shows the name a file will be saved under,
  and after phase 1 that is the rewritten name on Windows. Whichever lands
  second wires them together.

## Kickoff prompt

```text
Read docs/plans/cross-platform-plan-2026-09-14.md and AGENTS.md. Verify the file
and line references against the current source first. Start with phase 0 only:
add the OS matrix to the Rust CI job on a topic branch, push it, and record
what fails on macOS and Windows in the plan, dated, before fixing anything.
The archive invariants in AGENTS.md apply on every platform; any Windows name
rewriting touches one normal component and never introduces a separator.
```
