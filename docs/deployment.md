# Deployment

Requirements, configuration variables, Docker, and Kubernetes/GKE details for
self-hosting a Drop relay. Installing the
`drop` CLI needs none of this — see the root [README](../README.md).

## Requirements

- Rust 1.85 or newer (the minimum version supporting Rust 2024);
- Docker Compose, only for the containerized deployment.

## Run it from source

```bash
cargo run
```

The relay then answers on `http://127.0.0.1:8080`, with `/health`, `/ready`,
and `/metrics`. It serves no web page: the root answers 404 with one line
saying what the host is. The default bind address is `0.0.0.0:8080`; override
it with `DROP_BIND_ADDR=127.0.0.1:8080 cargo run`. See
[`commands.md`](commands.md) for other local workflows.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `DROP_BIND_ADDR` | `0.0.0.0:8080` | server socket address |
| `PORT` | unset | hosting-provider port, used when `DROP_BIND_ADDR` is unset |
| `DROP_TRUST_GCP_X_FORWARDED_FOR` | `false` | use the client IP appended by a trusted GCP Application Load Balancer |
| `DROP_SHUTDOWN_DRAIN_DELAY_SECS` | `10` | after SIGTERM, how long `/ready` reports `503` before draining, so load balancers stop sending new connections |
| `DROP_SHUTDOWN_MAX_TRANSFER_WAIT_SECS` | `3500` | after the drain delay, how long to keep running for in-flight transfers |
| `RUST_LOG` | application default | tracing filter |
| `DROP_SERVER` | none | relay the `drop` CLI forwards through; unset means the direct path only |
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

Build the relay and run it behind Caddy:

```bash
docker compose up --build
```

[`../Dockerfile`](../Dockerfile) builds the relay alone.

For a public host, set `DROP_SITE_ADDRESS` to the domain, point DNS at the
server, and optionally set `ACME_EMAIL`. Caddy then handles HTTPS
certificates.

## Behind a hosting provider

The relay honors a hosting provider's `PORT` variable when `DROP_BIND_ADDR` is
not set. No host serves the CLI installer: it is a release asset,
`https://github.com/op-q/drop/releases/latest/download/install.sh`, built from
[`../scripts/install.sh`](../scripts/install.sh).

**Removed in 0.4.0:** the browser client, the `Dockerfile.fullstack` image that
bundled it, the split frontend/backend deployment, and `DROP_ALLOWED_ORIGINS`,
whose only consumer the browser client was. A relay that still has the variable
set ignores it. See [`decisions.md`](decisions.md) entry 17.

## Kubernetes deployment

The [`../k8s`](../k8s/README.md) directory contains a portable base
Deployment and Service, a local `kind` overlay for learning Kubernetes, and a
GKE Autopilot overlay with an external Application Load Balancer,
Google-managed TLS, health checks, and connection draining.

Drop runs as exactly one pod because sessions and live transfer channels exist
only in process memory. [`../k8s/README.md`](../k8s/README.md) has the full
shutdown sequence, the Autopilot-specific shutdown budget, and the deploy
walk-through.
