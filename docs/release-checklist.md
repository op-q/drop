# Release checklist

The evidence gate for tagging a release. Drop is pre-release software: a passing
checklist means the candidate met this documented gate, not that the protocol or
deployment defaults are stable.

Record the revision, host platform, Rust version, and the exact
commands run. A check that was not run must be reported as not run rather than
assumed to pass.

Tagging `v*` publishes CLI binaries and the checksums file that `install.sh`
verifies against, so a mistake here reaches users through the install path.

## Repository safety

- [ ] The release is prepared on a topic branch, not `main`.
- [ ] Review `git status --short --branch` and the complete diff, staged and
      unstaged.
- [ ] No real session code, transferred file, client IP address, credential,
      populated environment file, or personal path is tracked.
- [ ] Run `scripts/check-secrets.sh`.
- [ ] Run `git diff --check`.
- [ ] New or updated dependencies were reviewed intentionally, not merged
      blind.

## Automated validation

```bash
scripts/check-secrets.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

- [ ] Formatting passes.
- [ ] Clippy passes across the workspace with warnings as errors.
- [ ] The full workspace test suite passes. Record the test count.
- [ ] CI is green on the release commit on Linux, macOS and Windows, and CodeQL
      is green.

All three workspace members must be covered: `api`, `drop-cli` and
`drop-crypto`. A root-package-only run is not sufficient.

## Transfer smoke test

Run against a locally built server, not the public instance. Use synthetic
files.

- [ ] CLI to CLI: `drop send` and `drop recv` complete, and the received file
      matches by checksum.
- [ ] A folder transfers, extracts, and reports the expected file count.
- [ ] A compressed send (`--compress`) completes, and the temporary file is
      gone afterward — including when the send is interrupted with Ctrl-C.
- [ ] Repeated small CLI transfers complete without `Connection reset by peer`.
      See item 1 in [`implementation-checklist.md`](implementation-checklist.md);
      until that is fixed, record the observed rate rather than claiming clean.

Failure paths:

- [ ] Cancelling from the sender is reported to the receiver.
- [ ] Disconnecting the receiver mid-transfer is reported to the sender.
- [ ] An unknown code is refused with `invalid session code`.
- [ ] A second receiver on a claimed session is refused.
- [ ] A transfer above the 4 GiB limit is refused at session creation.
- [ ] An extraction refusal — an absolute path, a `..` component, or an escaping
      symlink — is reported as a warning and does not write outside the
      destination.
- [ ] An existing file is not replaced without `--force`.

## Release artifacts

- [ ] The tag matches the version in `Cargo.toml`.
- [ ] `release.yml` built every target on its native runner and all jobs
      succeeded.
- [ ] The checksums file lists every published binary.
- [ ] `install.sh` installs the tagged version on a clean machine and its
      checksum verification passes.
- [ ] `DROP_VERSION` pins the previous release correctly, so an install can be
      rolled back.
- [ ] Release notes state what changed and any breaking protocol change.

## Documentation claims

- [ ] The README describes the actual release and its most important limits.
- [ ] [`protocol.md`](protocol.md) matches the implemented wire contract,
      including status strings and limits.
- [ ] [`security.md`](security.md) matches current behavior, and its known
      weaknesses list is still accurate.
- [ ] Drop is not described as peer-to-peer or end-to-end encrypted unless the
      corresponding decision in [`decisions.md`](decisions.md) has been made, and
      the claim names the relay fallback where it describes the direct path.
- [ ] Configuration tables list every variable the code reads.
- [ ] Operational limits reflect what is actually true, including anything
      newly broken for scripted use.
- [ ] [`implementation-checklist.md`](implementation-checklist.md) reflects
      honest status.

## Release decision

The release report should state:

- the revision and proposed version;
- test, lint, audit, and secret-scan results;
- which manual checks were performed, and on what platforms;
- checks skipped and why;
- known limitations and open defects, including anything in the known-weaknesses
  list that a user should know about; and
- whether the candidate is ready, conditionally ready, or not ready.
