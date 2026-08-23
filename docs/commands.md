# Commands

Short commands for humans and agents working on Drop. The authoritative
validation set lives in [AGENTS.md](../AGENTS.md); this file is the practical
index, including the things that are easy to forget.

## Full check (run before opening a pull request)

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

The repository is a Cargo workspace with four members — `api`, `drop-cli`,
`drop-crypto`, and `drop-crypto-wasm`. A root-package-only run misses all but
the relay, which is why every Rust command here is workspace-wide.

## The web build is not a pure Node build

`npm run build` compiles `crypto-wasm/` before it runs Vite, because the
browser runs the same envelope the CLI runs rather than a second
implementation of it ([`decisions.md`](decisions.md) entry 11). That needs a
Rust toolchain, the wasm32 target, and `wasm-pack`:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack        # or a release binary from rustwasm/wasm-pack
```

Without them `npm run build`, `npm test`, and `npm run dev` all fail at the
`build:wasm` step rather than at anything that mentions the browser.

`npm test` runs the envelope tests and, when `target/debug/api` and
`target/debug/drop` exist, the CLI-to-browser interoperation tests against a
real relay. It skips those rather than failing when the binaries are absent, so
run `cargo build --workspace --bins` first if you mean to exercise them.

## Secret scan

```bash
scripts/check-secrets.sh
```

Required before any publication. Also run `git diff --check`.

## Rust tests

```bash
cargo test --workspace --all-targets          # everything
cargo test -p api                             # relay only
cargo test -p drop-cli                        # CLI only
cargo test -p drop-cli --test transfer        # CLI transfer tests
```

## Run the relay locally

```bash
npm --prefix web ci
npm --prefix web run build
cargo run
```

Then `http://127.0.0.1:8080/`, with `/health`, `/ready`, and `/metrics`
alongside it.

Bind elsewhere with `DROP_BIND_ADDR=127.0.0.1:8080 cargo run`.

## Frontend development

```bash
cd web && VITE_BACKEND_ORIGIN=http://127.0.0.1:8080 npm run dev
```

The backend must allow the Vite origin:

```bash
DROP_ALLOWED_ORIGINS=http://127.0.0.1:5173 cargo run
```

## Local transfer

With a relay running, in two terminals:

```bash
cargo run -p drop-cli -- send ./some-folder --server http://127.0.0.1:8080
cargo run -p drop-cli -- recv <CODE> --server http://127.0.0.1:8080 --out /tmp/drop-test
```

Use synthetic files. Never point a test at the public instance.

## Container

```bash
docker compose up --build
```

[`Dockerfile.fullstack`](../Dockerfile.fullstack) builds the web client and the
relay; [`Dockerfile`](../Dockerfile) builds the backend only, for split
deployments.

## Kubernetes

See [`k8s/README.md`](../k8s/README.md). The GKE shutdown budget has its own
check:

```bash
scripts/check-gke-shutdown-budget.sh
```

## Metrics snapshot

```bash
curl -s http://127.0.0.1:8080/metrics
```

JSON, not Prometheus text. Zero `active_sessions` is what a clean shutdown or a
DNS cutover waits for.
