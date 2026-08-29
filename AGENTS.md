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
  product and threat-model decision. This covers the server; the CLI may use a
  local temporary file to compress a payload, and must delete it on every exit
  path.
- Treat archive entries received from a peer as hostile input. Extraction must
  refuse absolute paths, `..` components, and symlink targets that resolve
  outside the destination. Path safety must be judged against the filesystem,
  not only against path text: a lexical check alone misses an entry that
  escapes by traversing a symlink an earlier entry created. Extraction must not
  replace files the receiver already has unless it was asked to.
- Do not describe Drop as peer-to-peer.
- CLI-to-CLI transfers are end-to-end encrypted and may be described so. Browser
  transfers are encrypted in the browser but are only as strong as the code the
  site delivered, which defeats a passive operator and stored traffic but not a
  server that actively serves modified client code. Never describe the two cases
  in wording that blurs them. See `docs/decisions.md` entries 7 and 11.
- Treat active session codes, transferred bytes, filenames, IP addresses, and
  operational logs as sensitive.
- Preserve the one-sender, one-receiver session lifecycle and bounded resource
  controls.
- Do not add telemetry, external uploads, or third-party browser requests
  without explicit direction and documentation.
- Use synthetic test fixtures; never commit real credentials or private files.

## Documentation

- [`docs/`](docs/README.md) is the engineering reference. Read
  [`docs/protocol.md`](docs/protocol.md) before changing the wire contract and
  [`docs/security.md`](docs/security.md) before changing a trust boundary.
- Save a multi-step plan in full as
  `docs/plans/<topic>-plan-YYYY-MM-DD.md` before or as implementation starts.
  Never compress a plan to a chat summary: preserve phases, file lists, risks,
  validation steps, and open questions so any agent can resume with no chat
  history. The contract is in [`docs/plans/README.md`](docs/plans/README.md).
- Mirror plan status into
  [`docs/implementation-checklist.md`](docs/implementation-checklist.md) and
  update it honestly in the same change as the behavior.
- Record a costly or hard-to-reverse choice in
  [`docs/decisions.md`](docs/decisions.md) rather than only in a commit message.

## Development workflow

- Keep changes focused and include tests for behavior changes.
- Run Rust formatting, Clippy, and tests when a Rust toolchain is available.
- The repository is a Cargo workspace (`api` and `cli`); run workspace-wide
  checks, not just the root package.
- Build the web client after changing Svelte, TypeScript, HTML, or CSS. The
  web build compiles `crypto-wasm/` to WebAssembly first, so it needs a Rust
  toolchain and the `wasm32-unknown-unknown` target as well as Node.
- `tsc --noEmit` does not cover `.svelte` files. Changes to `App.svelte` are
  checked by the build and by tests, not by the type checker.
- Keep README claims aligned with tested behavior and deployment reality.
- Avoid committing caches and unrelated generated output.

## Commands

```bash
scripts/check-secrets.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
npm --prefix web ci
npm --prefix web run build
npm --prefix web run typecheck
npm --prefix web test
npm --prefix web audit --audit-level=high
```
