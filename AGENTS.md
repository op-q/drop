# Repository working rules

These rules apply to automated coding agents and human-assisted automation in
this repository.

## Git safety

- Never commit or push unless the user explicitly requests it.
- Never push directly to `main`; use a focused topic branch and pull request.
- Never force-push, rewrite history, delete branches, or discard user changes
  without explicit approval.
- Treat all pre-existing working-tree changes as user-owned.
- Before publication, inspect the complete diff and run
  `scripts/check-secrets.sh`.

## Product and security invariants

- Drop is an ephemeral relay. Do not add file persistence without an explicit
  product and threat-model decision.
- Do not describe Drop as peer-to-peer or end-to-end encrypted.
- Treat active session codes, transferred bytes, filenames, IP addresses, and
  operational logs as sensitive.
- Preserve the one-sender, one-receiver session lifecycle and bounded resource
  controls.
- Do not add telemetry, external uploads, or third-party browser requests
  without explicit direction and documentation.
- Use synthetic test fixtures; never commit real credentials or private files.

## Development workflow

- Keep changes focused and include tests for behavior changes.
- Run Rust formatting, Clippy, and tests when a Rust toolchain is available.
- Build the web client after changing Svelte, TypeScript, HTML, or CSS.
- Keep README claims aligned with tested behavior and deployment reality.
- Avoid committing caches and unrelated generated output.

## Commands

```bash
scripts/check-secrets.sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
npm --prefix web ci
npm --prefix web run build
npm --prefix web audit --audit-level=high
```
