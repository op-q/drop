# Drop

Drop is a real-time file transfer service built in Rust that streams data directly between a sender and a receiver using WebSockets. Files are never stored on the server, and each transfer is tied to a one-time session code that exists only while both parties are connected.

The system is designed as an ephemeral relay: the backend coordinates a live transfer between two clients without persisting data, enforcing strict session lifecycles and one-time access.

---

## Overview

Drop implements a minimal, high-performance backend for ephemeral file transfer. Instead of handling uploads and downloads through storage, it relays data in real time between connected peers.

The project focuses on backend architecture, async concurrency, and protocol design using Rust.

---

## Features

- Real-time file transfer over WebSockets
- One-time session codes
- No file storage (in-memory relay only)
- Single sender and single receiver per session
- Live transfer model (sender must remain connected)
- Immediate cancellation and error propagation
- Strict session lifecycle management
- 4 GB backend-enforced upload limit
- Per-IP rate limiting and WebSocket connection limiting
- Session TTL and background cleanup
- Progressive browser download when supported
- Built-in metrics and structured tracing

---

## Architecture

- **Language:** Rust  
- **Framework:** Axum  
- **Runtime:** Tokio  
- **Transport:** WebSockets  
- **State:** In-memory session store with explicit store/service boundaries
- **Coordination:** Channel-based message passing (`mpsc`)
- **Frontend:** Svelte + TypeScript, backend-driven UI
- **Observability:** `tracing`, HTTP trace layer, metrics snapshot endpoint

The backend maintains short-lived in-memory sessions keyed by one-time codes. Each session links:
- a sender connection
- a receiver connection
- communication channels for streaming data and control messages

---

## Transfer Flow

1. A session is created via `POST /api/session/create`
2. The sender connects to `/ws/upload/:code`
3. The receiver connects to `/ws/download/:code`
4. The backend binds sender and receiver to the same session
5. The sender streams file data in chunks
6. The backend relays chunks directly to the receiver
7. The session is destroyed after completion, cancellation, or disconnect

---

## Protocol

### Sender → Backend

- `meta`: file metadata (name, size, type)
- binary frames: file data
- `complete`: transfer finished
- `cancel`: abort transfer

### Backend → Sender

- `waiting_for_receiver`
- `receiver_connected`
- `sending`
- `transfer_complete`
- `cancelled`
- `error`

### Backend → Receiver

- `waiting_for_sender`
- `meta`
- binary frames
- `progress`
- `complete`
- `error`

---

## Concurrency Model

- Each WebSocket connection is split into independent send/receive tasks
- `mpsc` channels are used to decouple sender and receiver streams
- Shared state is kept in a small explicit in-memory store
- Sessions are short-lived and cleaned up after termination events
- WebSocket heartbeats and idle timeouts protect long-lived connections

---

## Running Locally

```bash
cd web
npm ci
npm run build

cd ..
cargo run
```

The app is served by Rust at:

- `http://127.0.0.1:8080/`
- `http://127.0.0.1:8080/health`
- `http://127.0.0.1:8080/metrics`

You can override the bind address with:

```bash
DROP_BIND_ADDR=0.0.0.0:8080 cargo run
```

---

## Docker Deployment

Build and run the full stack with:

```bash
docker compose up --build
```

This starts:

- the Rust app container
- a Caddy reverse proxy in front of it

Environment variables used by the compose setup:

- `DROP_SITE_ADDRESS`
  - Example: `drop.example.com`
- `ACME_EMAIL`
  - Example: `ops@example.com`
- `RUST_LOG`

For local testing, the compose file defaults to `localhost`. For a VPS, set `DROP_SITE_ADDRESS` to your real domain and point DNS to the server. Caddy will then handle HTTPS automatically.

Notes:

- [Dockerfile](/home/opq/code/drop/Dockerfile) is backend-only and is the right one for Render
- [Dockerfile.fullstack](/home/opq/code/drop/Dockerfile.fullstack) is used by local `docker compose` when you want Rust + built frontend + Caddy together

---

## Split Deployment

If you deploy the frontend and backend separately:

- deploy the Rust backend to Render
- deploy the Svelte frontend to Vercel

Set these environment variables:

- On Vercel:
  - `VITE_BACKEND_ORIGIN=https://your-render-service.onrender.com`
- On Render:
  - `DROP_ALLOWED_ORIGINS=https://your-vercel-project.vercel.app,https://your-domain.com`

Render also provides a `PORT` environment variable automatically, and the backend now binds to it when present.
For Render, use the backend-only [Dockerfile](/home/opq/code/drop/Dockerfile).

This is the cleanest setup if you want any two users to open the public page and use the same backend relay service.

---

## Operational Considerations

Drop is designed as an in-memory relay with no persistent storage. As such, it relies on active connections and bounded resource usage.

Current production-oriented safeguards:
- backend-enforced 4 GB upload limit
- concurrent session cap
- per-IP session creation rate limiting
- per-IP WebSocket connection limiting
- TTL-based session expiration
- progress/error propagation between peers
- metrics snapshot endpoint
- structured logging and request tracing

Current limitations:
- Transfers are live and require both peers to remain connected
- Sessions are stored in memory and are short-lived
- Large transfers still depend on network stability and receiver throughput
- Resume, retry, and chunk acknowledgments are not implemented
- The in-memory store means a single process remains the source of truth
- Metrics are exposed as JSON snapshots rather than Prometheus-style scraping

---

## License

MIT
