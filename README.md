# Drop

[![CI](https://github.com/op-q/drop/actions/workflows/ci.yml/badge.svg)](https://github.com/op-q/drop/actions/workflows/ci.yml)
[![CodeQL](https://github.com/op-q/drop/actions/workflows/codeql.yml/badge.svg)](https://github.com/op-q/drop/actions/workflows/codeql.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Drop is a small, self-hostable file-transfer service built with Rust and
Svelte. A sender and receiver connect to the same short-lived session over
WebSockets while the server relays file chunks directly between them. Drop
does not write transferred files to application storage.

Try the hosted instance at [drop.lifbom.com](https://drop.lifbom.com).

> [!IMPORTANT]
> Drop is pre-release software. The protocol and deployment defaults may
> change, and the public instance should not be treated as a durable storage or
> high-assurance secure-transfer service.

## Features

- live WebSocket transfer with no application-level file persistence;
- one-time, short-lived transfer sessions;
- one sender and one receiver per session;
- backend-enforced 4 GiB transfer limit;
- per-IP session and WebSocket connection limits;
- cancellation, disconnect, timeout, and error propagation;
- progressive browser downloads when the File System Access API is available;
- in-memory metrics and structured tracing;
- a Svelte and TypeScript browser client;
- Docker and Caddy configuration for self-hosting.

## Privacy and security model

Drop is an ephemeral relay, not peer-to-peer or end-to-end encrypted storage.
When HTTPS is configured, TLS protects each connection to the deployment, but
the Drop server still handles file bytes in memory while relaying them. The
server operator and a compromised server could therefore access an active
transfer.

The session code acts as a temporary capability: anyone who learns an active
code may be able to join that session. Share codes through a trusted channel.
Drop removes sessions after completion, cancellation, disconnect, or the
five-minute TTL, but operating-system, proxy, and infrastructure behavior is
outside the application's no-storage guarantee.

Please report vulnerabilities privately according to the
[security policy](.github/SECURITY.md).

## Architecture

| Layer | Technology | Responsibility |
| --- | --- | --- |
| API | Rust, Axum | HTTP endpoints, WebSocket upgrades, static frontend |
| Runtime | Tokio | async connections, channels, timeouts, cleanup |
| State | in-memory store | active session metadata and connection channels |
| Web | Svelte, TypeScript, Vite | sender and receiver browser flows |
| Edge | Caddy | optional HTTPS termination and reverse proxy |

Each session connects:

- a sender WebSocket;
- a receiver WebSocket;
- bounded Tokio channels for file chunks, progress, and control events.

The transfer flow is:

1. `POST /api/session/create` creates a temporary session code.
2. The sender connects to `/ws/upload/:code`.
3. The receiver connects to `/ws/download/:code`.
4. The sender provides metadata and streams binary frames.
5. Drop forwards those frames to the receiver.
6. Drop destroys the session after a terminal event.

## Requirements

- Rust 1.85 or newer (the minimum version supporting Rust 2024);
- Node.js `^20.19.0` or `>=22.12.0`;
- npm;
- Docker Compose, only for the containerized deployment.

## Run locally

Install and build the browser client:

```bash
cd web
npm ci
npm run build
cd ..
```

Start the Rust server:

```bash
cargo run
```

Drop is then available at:

- `http://127.0.0.1:8080/`;
- `http://127.0.0.1:8080/health`;
- `http://127.0.0.1:8080/metrics`.

The default bind address is `0.0.0.0:8080`. Override it when needed:

```bash
DROP_BIND_ADDR=127.0.0.1:8080 cargo run
```

For frontend development, run Vite separately:

```bash
cd web
VITE_BACKEND_ORIGIN=http://127.0.0.1:8080 npm run dev
```

Then allow the Vite origin on the backend:

```bash
DROP_ALLOWED_ORIGINS=http://127.0.0.1:5173 cargo run
```

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `DROP_BIND_ADDR` | `0.0.0.0:8080` | server socket address |
| `PORT` | unset | hosting-provider port, used when `DROP_BIND_ADDR` is unset |
| `DROP_ALLOWED_ORIGINS` | none | comma-separated cross-origin frontend URLs |
| `RUST_LOG` | application default | tracing filter |
| `VITE_BACKEND_ORIGIN` | current page origin | backend URL for a separately hosted frontend |
| `DROP_SITE_ADDRESS` | `localhost` | Caddy site address in Docker Compose |
| `ACME_EMAIL` | empty | optional Caddy ACME account email |

Copy [`.env.example`](.env.example) when configuring Docker Compose. The Rust
application does not automatically load `.env` files; export its variables in
your shell or deployment environment.

## Docker deployment

Build the complete app and run it behind Caddy:

```bash
docker compose up --build
```

[`Dockerfile.fullstack`](Dockerfile.fullstack) builds both the Svelte frontend
and Rust service. [`Dockerfile`](Dockerfile) builds the backend only for split
deployments.

For a public host, set `DROP_SITE_ADDRESS` to the domain, point DNS at the
server, and optionally set `ACME_EMAIL`. Caddy then handles HTTPS certificates.

## Split deployment

The frontend and backend can be deployed independently:

- set `VITE_BACKEND_ORIGIN=https://api.example.com` on the frontend;
- set `DROP_ALLOWED_ORIGINS=https://drop.example.com` on the backend;
- use the backend-only [`Dockerfile`](Dockerfile).

The backend honors a hosting provider's `PORT` variable when
`DROP_BIND_ADDR` is not set.

## Development

Run the repository checks before opening a pull request:

```bash
scripts/check-secrets.sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
npm --prefix web ci
npm --prefix web run build
npm --prefix web audit --audit-level=high
```

The main directories are:

```text
src/        Rust application, domain, services, routes, and telemetry
tests/      integration and WebSocket transfer tests
web/        Svelte and TypeScript client
.github/    community files, issue forms, and CI/security automation
scripts/    repository safety checks
```

See [CONTRIBUTING.md](.github/CONTRIBUTING.md) for the branch workflow and pull
request expectations.

## Operational limits

- Transfers require both peers to remain online.
- Sessions and metrics are local to one process.
- Horizontal scaling needs session affinity or a shared coordination layer.
- Resume, retry, and chunk acknowledgements are not implemented.
- Browser fallback downloads buffer the complete file in memory.
- The metrics endpoint returns a JSON snapshot rather than Prometheus text.
- Reverse proxies must preserve the intended client-address semantics for
  per-IP limits.

## License

Drop is available under the [MIT License](LICENSE).
