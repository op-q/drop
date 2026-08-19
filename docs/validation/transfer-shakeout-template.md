# Transfer shakeout template

Last checked: 2026-08-19

Use this when the owner asks for a deep exercise of a running relay — "try
transfers as hard as you can and tell me what breaks" — or before a release.
It produces a dated, numbered report under `docs/validation/`.

This is exploratory testing with a written record, not a substitute for the
test suite or for [`../release-checklist.md`](../release-checklist.md).

## Invocation prompt

```text
Run a full Drop transfer shakeout. Build and run a local relay, exercise
browser and CLI transfers in every pairing, test the failure and abuse paths,
then write a dated numbered report under docs/validation. Prioritize
truthfulness about what you actually observed, the no-persistence guarantee,
the resource bounds, and whether errors reach the right peer with a
comprehensible message.
```

For the next numbered run use:

```text
docs/validation/transfer-shakeout-<NNN>-YYYY-MM-DD.md
```

## Required setup

Before testing:

- Read [AGENTS.md](../../AGENTS.md) and [`../security.md`](../security.md).
- Run `git status --short --branch` and preserve unrelated working-tree changes.
- Build the web client and run the relay locally. Never run a shakeout against
  the public instance.
- Use synthetic files only. Generate them; do not use personal data.
- Do not put a real session code, filename, or IP address in the report.

Recommended environment:

```bash
DROP_BIND_ADDR=127.0.0.1:8080
DROP_ALLOWED_ORIGINS=http://127.0.0.1:5173
RUST_LOG=drop=debug
```

## Test lanes

### 1. Baseline

- `GET /health`, `GET /ready`, `GET /metrics`.
- `POST /api/session/create` with a valid body.

Record: the metrics snapshot shape, and whether `/ready` and `/health` mean
different things as documented.

### 2. The four pairings

Transfer a small synthetic file in each direction:

- browser to browser
- CLI to CLI
- browser to CLI
- CLI to browser

Record: checksum match at the receiving end, wall-clock duration, and whether
progress reporting looked sane in each client.

### 3. Payload shapes

- A single file well under a chunk.
- A file that is exactly a chunk multiple, and one that is not.
- A folder with many small files.
- A folder containing a deep path, an empty directory, and a symlink.
- A compressed send (`--compress`) and the same payload uncompressed.
- Something already compressed, to see the cost of compressing it anyway.

Record: declared size versus received size, extracted file count, and whether
the CLI's temporary compression file is gone afterward — including after
Ctrl-C mid-send.

### 4. Scale and limits

- A transfer near the 4 GiB ceiling, and one over it.
- Several concurrent transfers.
- Enough concurrent sessions to approach the 100-session cap.
- More than four WebSocket connections from one address.
- Rapid repeated session creation, to hit the per-minute limit.

Record: which limit fired, what each peer was told, and whether the relay
stayed healthy afterward.

### 5. Failure paths

- Sender cancels mid-transfer.
- Receiver disconnects mid-transfer.
- Sender disconnects mid-transfer.
- Network interruption on each side.
- An unknown session code.
- A second sender and a second receiver on the same session.
- A session left idle past its lifetime.

Record: which peer learned what, how quickly, and whether the message was
comprehensible to a person rather than only to a developer.

### 6. Repeated small transfers

Run at least 50 consecutive small CLI transfers.

Record: the count of `Connection reset by peer` or any other spurious error on
a transfer that otherwise succeeded. See
[`../plans/relay-teardown-drain-plan-2026-08-19.md`](../plans/relay-teardown-drain-plan-2026-08-19.md);
until that lands, report the observed rate rather than treating it as new.

### 7. Hostile archive

Build synthetic archives that attempt to escape the destination:

- an absolute path;
- a `..` traversal;
- a symlink whose target leaves the destination;
- an entry whose parent directory is a symlink created by an earlier entry;
- setuid, setgid, and sticky permission bits;
- an entry that collides with an existing file, with and without `--force`;
- a highly compressible payload approaching the expansion limit.

Record: for each, whether it was refused, whether the refusal was a warning
that let extraction continue, and — checked directly on the filesystem —
whether anything landed outside the destination.

### 8. Hostile display input

Send files whose names contain control characters, an ANSI escape sequence, a
right-to-left override, and an extreme length.

Record: exactly what the terminal and the browser rendered. This lane matters
more once the confirmation prompt exists; see
[`../plans/receiver-confirmation-plan-2026-08-19.md`](../plans/receiver-confirmation-plan-2026-08-19.md).

### 9. Shutdown behavior

Send `SIGTERM` to the relay during an active transfer.

Record: whether `/ready` reports 503 before draining, whether the in-flight
transfer completed or failed, and what each peer was told.

### 10. Leak check

After the run, with the relay still up:

- confirm `/metrics` reports zero active sessions;
- confirm no transferred file content exists in application storage;
- review the debug logs for anything that should be treated as sensitive.

Record: anything logged that [`../security.md`](../security.md) calls
sensitive — filenames, codes, IP addresses — and whether its log level is
appropriate.

## Report requirements

Create `docs/validation/transfer-shakeout-<NNN>-YYYY-MM-DD.md` containing:

- date, run number, and revision tested;
- scope and what was deliberately not tested;
- environment summary, with no secret values;
- what worked well;
- findings graded P0 / P1 / P2;
- exact evidence — commands, observed output, byte counts;
- code anchors for anything actionable;
- recommended fix order;
- retest criteria for the next run.

Also update:

- [`../implementation-checklist.md`](../implementation-checklist.md) if findings
  create or change planned work;
- the relevant file in [`../plans/`](../plans/) if a finding belongs to one;
- [`../security.md`](../security.md) if a new weakness is found;
- [`../README.md`](../README.md) if a new report type is introduced.

## Stop conditions

Stop and report rather than continuing if:

- the relay cannot be built or started after reasonable attempts;
- a lane would need a real credential, a real personal file, or the public
  instance;
- a destructive command outside a temporary directory would be required;
- a P0 finding makes further lanes meaningless.

Before finishing:

- stop every relay process started for the run;
- state which lanes were not run and why;
- state whether any code changed, or whether the run was observation only.
