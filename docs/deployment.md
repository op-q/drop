# Deployment

Requirements, configuration variables, Docker, split frontend/backend
deployment, and Kubernetes/GKE details for self-hosting Drop. Installing the
`drop` CLI needs none of this — see the root [README](../README.md).

## Requirements

- Rust 1.85 or newer (the minimum version supporting Rust 2024);
- Node.js `^20.19.0` or `>=22.12.0`;
- npm;
- Docker Compose, only for the containerized deployment.

## Run it from source

```bash
cd web && npm ci && npm run build && cd ..
cargo run
```

Drop is then available at `http://127.0.0.1:8080/`, with `/health`, `/ready`,
and `/metrics` alongside it. The default bind address is `0.0.0.0:8080`;
override it with `DROP_BIND_ADDR=127.0.0.1:8080 cargo run`. See
[`commands.md`](commands.md) for frontend dev-server setup and other local
workflows.

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
| `DROP_SERVER` | `https://api.drop.lifbom.com` | relay used by the `drop` CLI |
| `DROP_TRANSPORT` | `auto` | carrier the `drop` CLI uses: `p2p`, `relay`, or `auto` |
| `DROP_RENDEZVOUS_RELAY` | n0's public relays | iroh relay the direct path becomes reachable through, as `http://host:port` |
| `DROP_RENDEZVOUS_BOOTSTRAP` | the public mainline routers | comma-separated `host:port` DHT nodes the direct path publishes to |
| `DROP_INSTALL_DIR` | `~/.local/bin` | where `install.sh` puts the CLI |
| `DROP_VERSION` | `latest` | release tag `install.sh` installs |
| `DROP_RELEASE_BASE` | GitHub releases | base URL `install.sh` downloads from |
| `DROP_SITE_ADDRESS` | `localhost` | Caddy site address in Docker Compose |
| `ACME_EMAIL` | empty | optional Caddy ACME account email |

Copy [`../.env.example`](../.env.example) when configuring Docker Compose. The
Rust application does not automatically load `.env` files; export its
variables in your shell or deployment environment.

### Self-hosting rendezvous

The two `DROP_RENDEZVOUS_*` variables are for a deployment whose machines cannot
reach the public internet. `DROP_SERVER` already lets a network run its own Drop
relay, but the *direct* path reached n0's relays and the mainline DHT or it did
not happen — so inside an egress-filtered network `--transport p2p` could only
fail and `auto` could only fall back. Pointing both variables at infrastructure
you run closes that.

A malformed value is an error rather than a quiet return to the public default,
which is deliberate: the point of setting them is keeping rendezvous inside your
network, and a typo that published to the public DHT instead would lose exactly
that, silently. Give the relay URL its scheme — `relay.example:3340` is a valid
URL whose *scheme* is `relay.example`, and it is refused for that reason.

What an operator takes on is in [`security.md`](security.md#self-hosted-rendezvous);
the decision is [`decisions.md`](decisions.md) entry 15. Neither variable relaxes
the address filter: a record still names a routable address or a relay, never a
private address of the sender's.

## Docker deployment

Build the complete app and run it behind Caddy:

```bash
docker compose up --build
```

[`../Dockerfile.fullstack`](../Dockerfile.fullstack) builds both the Svelte
frontend and Rust service. [`../Dockerfile`](../Dockerfile) builds the backend
only for split deployments.

For a public host, set `DROP_SITE_ADDRESS` to the domain, point DNS at the
server, and optionally set `ACME_EMAIL`. Caddy then handles HTTPS
certificates.

## Split deployment

The frontend and backend can be deployed independently:

- set `VITE_BACKEND_ORIGIN=https://api.example.com` on the frontend;
- set `DROP_ALLOWED_ORIGINS=https://drop.example.com` on the backend;
- use the backend-only [`../Dockerfile`](../Dockerfile).

The backend honors a hosting provider's `PORT` variable when
`DROP_BIND_ADDR` is not set. The backend-only Dockerfile does not build the
web client, so a split deployment serves `/install.sh` from the frontend host
rather than from the API host.

## Kubernetes deployment

The [`../k8s`](../k8s/README.md) directory contains a portable base
Deployment and Service, a local `kind` overlay for learning Kubernetes, and a
GKE Autopilot overlay with an external Application Load Balancer,
Google-managed TLS, health checks, and connection draining.

Drop runs as exactly one pod because sessions and live transfer channels exist
only in process memory. [`../k8s/README.md`](../k8s/README.md) has the full
shutdown sequence, the Autopilot-specific shutdown budget, and the deploy
walk-through.
