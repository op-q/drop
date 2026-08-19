# Drop

[![CI](https://github.com/op-q/drop/actions/workflows/ci.yml/badge.svg)](https://github.com/op-q/drop/actions/workflows/ci.yml)
[![CodeQL](https://github.com/op-q/drop/actions/workflows/codeql.yml/badge.svg)](https://github.com/op-q/drop/actions/workflows/codeql.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Drop is a small, self-hostable file-transfer service built with Rust and
Svelte. A sender and receiver connect to the same short-lived session over
WebSockets while the server relays file chunks directly between them. Drop
does not write transferred files to application storage.

Transfers work between two browsers, between two terminals with the `drop`
command-line client, or between one of each.

Try the hosted instance at [drop.lifbom.com](https://drop.lifbom.com).

> [!IMPORTANT]
> Drop is pre-release software. The protocol and deployment defaults may
> change, and the public instance should not be treated as a durable storage or
> high-assurance secure-transfer service.

## Features

- live WebSocket transfer with no application-level file persistence;
- one-time, short-lived transfer sessions;
- one sender and one receiver per session;
- a command-line client that sends a whole folder as one transfer;
- backend-enforced 4 GiB transfer limit;
- per-IP session and WebSocket connection limits;
- cancellation, disconnect, timeout, and error propagation;
- 1 MiB chunks with a windowed, acknowledged flow-control loop;
- a server-wide ceiling on buffered bytes, shared across sessions;
- progressive browser downloads when the File System Access API is available;
- in-memory metrics and structured tracing;
- a Svelte and TypeScript browser client;
- Docker and Caddy configuration for self-hosting.

## Command-line client

Install the `drop` client on both computers:

```bash
curl -fsSL https://drop.lifbom.com/install.sh | sh
```

The script picks the right prebuilt binary for the platform, checks it against
the published SHA-256 checksums, and installs it to `~/.local/bin`. Set
`DROP_INSTALL_DIR` to install elsewhere, or `DROP_VERSION` to pin a release.
The checksums come from the same release as the binary, so they detect a
corrupted or truncated download rather than a compromised release; the trust
anchor is GitHub, and the artifacts are not signed.

On the sending computer, point it at a file or a folder:

```bash
$ drop send ./project
Sending ./project (128 files, archived as project.tar)
Size    412.7 MiB

  Run this on the other computer:

      drop recv 7F2A91

Waiting for the receiver to connect...
```

On the receiving computer, use the code:

```bash
$ drop recv 7F2A91
Receiving project.tar (412.7 MiB)
Receiving  100.0%  412.7 MiB / 412.7 MiB  86.4 MiB/s  ETA --
Extracted 128 files into .
```

A folder is streamed as a tar archive and unpacked on arrival; a single file is
written straight to disk. The code is printed on stdout by itself, so it can be
piped, while the progress display goes to stderr.

Nothing that already exists in the destination is replaced unless `--force` is
given: a skipped entry is reported and extraction continues. The sender picks
the paths inside the archive, so overwriting what the receiver already has is
not a decision the other end gets to make silently.

| Option | Applies to | Purpose |
| --- | --- | --- |
| `-s`, `--server <URL>` | both | relay to use, or set `DROP_SERVER` |
| `-c`, `--compress` | `send` | compress before sending |
| `--level <N>` | `send` | compression level, 1-9 (default 6) |
| `-o`, `--out <DIR>` | `recv` | where to write (default: current directory) |
| `--no-extract` | `recv` | save the archive instead of unpacking it |
| `-f`, `--force` | `recv` | replace files that already exist |

Compression is off by default because it cannot be streamed: Drop declares the
exact payload length before sending a byte, so a compressed payload is written
to a temporary file first to learn its length. That costs local disk and a
second pass, which is worth it for source trees and documents but wasted on
media that is already compressed. The temporary file is created readable only
by its owner and is removed when the transfer ends, however it ends, including
when the send is interrupted with Ctrl-C.

To point the client at a self-hosted relay:

```bash
drop send ./project --server https://drop.example.com
```

### Receiving from an untrusted sender

The receiving end treats an archive as hostile input, because the sender chooses
every path inside it:

- absolute paths, `..` components, and Windows drive prefixes are refused;
- an entry is refused if any of its parent directories on disk is a symbolic
  link, which is what stops a chain of links from walking the extractor out of
  the destination even when every path is lexically clean;
- a symlink is refused if its target leaves the destination, evaluated against
  what is on disk rather than against the target's text alone;
- existing files are kept unless `--force` is given;
- permission bits are masked to the ownership bits, so an archive cannot set
  setuid, setgid, or sticky;
- a compressed payload that expands more than a hundredfold is abandoned rather
  than unpacked, which bounds a decompression bomb by the amount the sender had
  to push through the relay.

Refusals are reported as warnings and do not abort the rest of the extraction.

## Privacy and security model

Drop is an ephemeral relay, not peer-to-peer or end-to-end encrypted storage.
When HTTPS is configured, TLS protects each connection to the deployment, but
the Drop server still handles file bytes in memory while relaying them. The
server operator and a compromised server could therefore access an active
transfer.

The session code acts as a temporary capability: anyone who learns an active
code may be able to join that session. Share codes through a trusted channel.
Drop removes sessions after completion, cancellation, disconnect, or five
minutes without transfer activity, but operating-system, proxy, and
infrastructure behavior is outside the application's no-storage guarantee.

Please report vulnerabilities privately according to the
[security policy](.github/SECURITY.md).

## Architecture

| Layer | Technology | Responsibility |
| --- | --- | --- |
| API | Rust, Axum | HTTP endpoints, WebSocket upgrades, static frontend |
| Runtime | Tokio | async connections, channels, timeouts, cleanup |
| State | in-memory store | active session metadata and connection channels |
| Web | Svelte, TypeScript, Vite | sender and receiver browser flows |
| Edge | Caddy or GKE Ingress | optional HTTPS termination and reverse proxy |

Each session connects:

- a sender WebSocket;
- a receiver WebSocket;
- bounded Tokio channels for file chunks, progress, and control events.

The transfer flow is:

1. `POST /api/session/create` creates a temporary session code.
2. The sender connects to `/ws/upload/:code`.
3. The receiver connects to `/ws/download/:code`.
4. The sender provides metadata and streams binary frames.
5. Drop forwards those frames to the receiver using a bounded in-flight window.
6. The receiver acknowledges chunks after successful file writes.
7. Drop reports success and destroys the session only after the receiver closes
   the completed file and confirms the final byte count.

### Transfer throughput

Throughput on a long-distance link is set by how many bytes may be in flight
before the sender must wait for an acknowledgement, so the window matters more
than raw bandwidth: roughly `window / round-trip time`. Clients send 1 MiB
chunks and keep up to 16 MiB unacknowledged, and acknowledge in 4 MiB batches
rather than per chunk.

Buffered bytes are bounded server-wide rather than per session. Each chunk
waiting inside the relay holds a reservation against one 200 MiB ceiling, so a
single transfer may use a large window while a hundred concurrent transfers
share that ceiling instead of multiplying it. A reservation is returned when
the chunk reaches the receiver, and also when a session is discarded, so an
abandoned transfer cannot strand capacity.

Progress notifications are advisory and throttled to one every 200 ms, instead
of one per chunk in each direction.

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
- `http://127.0.0.1:8080/ready`;
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
| `DROP_TRUST_GCP_X_FORWARDED_FOR` | `false` | use the client IP appended by a trusted GCP Application Load Balancer |
| `DROP_SHUTDOWN_DRAIN_DELAY_SECS` | `10` | after SIGTERM, how long `/ready` reports `503` before draining, so load balancers stop sending new connections |
| `DROP_SHUTDOWN_MAX_TRANSFER_WAIT_SECS` | `3500` | after the drain delay, how long to keep running for in-flight transfers |
| `RUST_LOG` | application default | tracing filter |
| `VITE_BACKEND_ORIGIN` | current page origin | backend URL for a separately hosted frontend |
| `DROP_SERVER` | `https://drop.lifbom.com` | relay used by the `drop` CLI |
| `DROP_INSTALL_DIR` | `~/.local/bin` | where `install.sh` puts the CLI |
| `DROP_VERSION` | `latest` | release tag `install.sh` installs |
| `DROP_RELEASE_BASE` | GitHub releases | base URL `install.sh` downloads from |
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

## Kubernetes deployment

The [`k8s`](k8s/README.md) directory contains:

- a portable base Deployment and Service;
- a local `kind` overlay for learning Kubernetes;
- a GKE Autopilot overlay with an external Application Load Balancer,
  Google-managed TLS, health checks, and connection draining.

Drop currently runs as exactly one pod because sessions and live transfer
channels exist only in process memory. The Deployment uses a `Recreate`
strategy so senders and receivers are not split across old and new pods during
a rollout. The server reports Kubernetes readiness separately from liveness
and drains established WebSockets on `SIGTERM`.

For a migration from an existing Fly.io deployment, first use a staging domain
to verify GKE. Keep Fly.io serving the public domain during that test, but do
not load-balance one domain across both deployments because their session
stores are independent. Switch DNS when `/metrics` reports zero active
sessions, then retire Fly.io after the DNS transition is stable.

## Development

Run the repository checks before opening a pull request:

```bash
scripts/check-secrets.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
npm --prefix web ci
npm --prefix web run build
npm --prefix web audit --audit-level=high
```

The main directories are:

```text
src/        Rust relay: domain, services, routes, and telemetry
cli/        `drop` command-line client and its archive format
tests/      integration and WebSocket transfer tests
web/        Svelte and TypeScript client, and the hosted install script
k8s/        portable and GKE-specific Kubernetes manifests
docs/       plans, protocol, security model, decisions, and checklists
.github/    community files, issue forms, and CI/security automation
scripts/    repository safety checks
```

The repository is a Cargo workspace: `api` is the relay and `drop-cli` is the
client. Only `api` is built into the container image; the client ships as a
prebuilt binary attached to a GitHub release by
[`release.yml`](.github/workflows/release.yml), which builds each target on a
native runner. Tagging `v*` publishes those binaries and the checksums file
that `install.sh` verifies against.

The [`docs`](docs/README.md) directory holds the engineering reference: the
[transfer protocol](docs/protocol.md), the [security model](docs/security.md),
the [decisions](docs/decisions.md) behind the current shape, the
[implementation checklist](docs/implementation-checklist.md) and the
[plans](docs/plans/README.md) behind it, the
[release checklist](docs/release-checklist.md), and the
[commands](docs/commands.md).

See [CONTRIBUTING.md](.github/CONTRIBUTING.md) for the branch workflow and pull
request expectations.

## Operational limits

- Transfers require both peers to remain online.
- Sessions and metrics are local to one process.
- A folder is sent as a single archive whose length is computed before the
  transfer starts. A file that changes size while it is being read is padded or
  truncated to the length recorded at scan time, with a warning, because the
  declared total is already committed.
- Sockets, FIFOs, and device nodes are skipped when archiving a folder.
- Four WebSocket connections are allowed per IP address, so one address can run
  two simultaneous transfers.
- The backend-only [`Dockerfile`](Dockerfile) does not build the web client, so
  a split deployment serves `/install.sh` from the frontend host rather than
  from the API host.
- The Kubernetes deployment intentionally uses one replica.
- Horizontal scaling needs shared session coordination and transfer-aware
  routing; ordinary session affinity alone cannot recover live WebSockets.
- Resume and retry are not implemented.
- Browsers without direct-to-disk download support buffer the complete file in
  memory and are limited to 256 MiB by the web client.
- The metrics endpoint returns a JSON snapshot rather than Prometheus text.
- Reverse proxies must preserve the intended client-address semantics for
  per-IP limits.

## License

Drop is available under the [MIT License](LICENSE).
