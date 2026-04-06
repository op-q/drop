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

---

## Architecture

- **Language:** Rust  
- **Framework:** Axum  
- **Runtime:** Tokio  
- **Transport:** WebSockets  
- **State:** In-memory session store (`Arc<Mutex<HashMap<...>>>`)  
- **Coordination:** Channel-based message passing (`mpsc`)

The backend maintains a session map keyed by short-lived codes. Each session links:
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
- `complete`
- `error`

---

## Concurrency Model

- Each WebSocket connection is split into independent send/receive tasks
- `mpsc` channels are used to decouple sender and receiver streams
- Shared state is managed through `Arc<Mutex<...>>`
- Sessions are short-lived and cleaned up after termination events

---

## Running Locally

```bash
cargo run
```

---

## Operational Considerations

Drop is designed as an in-memory relay with no persistent storage. As such, it relies on active connections and bounded resource usage.

In its current form:
- Transfers are live and require both peers to remain connected
- Sessions are stored in memory and are short-lived
- Large transfers depend on network stability and receiver throughput

The system is intended to be extended with:
- resource limits (file size, session count)
- rate limiting
- session expiration and cleanup
- backpressure handling for slow receivers

---

## License

MIT