# Transfer protocol

This is the contract between a Drop client and the relay, as currently
implemented. It exists so a third client can interoperate and so a change to the
wire format is a deliberate act rather than a side effect.

Everything here describes shipped behavior. Planned changes are collected at the
end and are not implemented.

## HTTP endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/api/session/create` | Create a transfer session and get its code |
| `GET` | `/ws/upload/{code}` | Sender WebSocket upgrade |
| `GET` | `/ws/download/{code}` | Receiver WebSocket upgrade |
| `GET` | `/health` | Liveness |
| `GET` | `/ready` | Readiness; reports `503` while draining |
| `GET` | `/metrics` | JSON snapshot, not Prometheus text |

### Session creation

Request:

```json
{ "filename": "project.tar", "file_size": 432539136 }
```

Response:

```json
{ "code": "7F2A91" }
```

The relay rejects an empty filename, a zero `file_size`, a `file_size` above the
4 GiB limit, a request beyond the per-IP session-creation rate limit, a request
once 100 concurrent sessions exist, and any request while the process is
draining for shutdown.

The code is six uppercase hexadecimal characters. See
[`security.md`](security.md) for what that does and does not protect.

### Metrics snapshot

```json
{
  "active_sessions": 0,
  "active_ws_connections": 0,
  "total_sessions_created": 0,
  "total_sessions_expired": 0,
  "total_transfers_completed": 0,
  "total_transfer_failures": 0,
  "total_bytes_relayed": 0
}
```

## Session lifecycle

```text
POST /api/session/create      -> code
sender connects  /ws/upload/{code}
receiver connects /ws/download/{code}
sender sends meta             -> relay forwards meta to receiver
sender streams binary chunks  -> relay forwards chunks to receiver
receiver acknowledges bytes   -> relay releases the sender's window
sender sends complete         -> relay tells receiver the stream ended
receiver confirms byte count  -> relay reports success and destroys the session
```

Exactly one sender and one receiver may join a session. A second sender is
refused with `sender already connected`; a second receiver with
`session already claimed`. An unknown or expired code is refused with
`invalid session code`.

The two peers may connect in either order. A sender that connects first is told
`waiting_for_receiver`; a receiver that connects first is told
`waiting_for_sender`.

## Sender to relay

Text frames, tagged JSON:

| Message | Fields | Meaning |
| --- | --- | --- |
| `meta` | `filename`, `file_size`, `mime_type` | Describes the payload; must precede any binary frame |
| `complete` | — | All declared bytes have been sent |
| `cancel` | — | Abandon the transfer |

Binary frames carry file bytes. A binary frame before `meta` is answered with an
error and ignored.

`meta.file_size` must equal the `file_size` given at session creation. A
mismatch fails the session. This is what stops a sender from reserving a small
session and then streaming something much larger.

## Relay to sender

Text frames, tagged JSON:

| Message | Fields |
| --- | --- |
| `status` | `status` |
| `progress` | `bytes_transferred`, `total_bytes` |
| `ack` | `bytes_received` |
| `error` | `message` |

Status values, in the order a successful transfer sees them:

| Status | Meaning |
| --- | --- |
| `waiting_for_receiver` | Sender connected first |
| `receiver_connected` | Both peers are present; clients treat this as the signal to start sending |
| `sending` | The relay accepted `meta` and is relaying |
| `awaiting_receiver` | The sender finished; the receiver is still writing |
| `transfer_complete` | The receiver confirmed the full byte count |
| `cancelled` | The transfer was abandoned |

`transfer_complete` and `cancelled` are terminal: the relay follows them with a
Close frame.

## Relay to receiver

Text frames, tagged JSON, plus binary frames carrying file bytes:

| Message | Fields |
| --- | --- |
| `status` | `status` — `waiting_for_sender` |
| `meta` | `filename`, `file_size`, `mime_type` |
| `progress` | `bytes_transferred`, `total_bytes` |
| `complete` | — |
| `error` | `message` |

`meta` always precedes the first binary frame. A receiver that sees bytes first
should treat it as a protocol violation.

## Receiver to relay

Text frames, tagged JSON:

| Message | Fields | Meaning |
| --- | --- | --- |
| `chunk_ack` | `bytes_received` | Cumulative bytes written to disk |
| `complete` | `bytes_received` | Final byte count after closing the file |
| `error` | — | The receiver is abandoning the transfer |

`bytes_received` is cumulative, not per chunk. The relay only reports success
after the receiver's final count matches the declared size, which is why a
transfer is confirmed by the receiver rather than assumed by the sender.

## Framing, flow control, and limits

| Property | Value | Source |
| --- | --- | --- |
| Recommended chunk size | 1 MiB | `RECOMMENDED_CHUNK_BYTES` |
| Maximum accepted frame | 1 MiB + 64 KiB | `WS_MAX_MESSAGE_BYTES` |
| Transfer size limit | 4 GiB | `MAX_UPLOAD_SIZE_BYTES` |
| Concurrent sessions | 100 | `MAX_CONCURRENT_SESSIONS` |
| WebSocket connections per IP | 4 | `MAX_WS_CONNECTIONS_PER_IP` |
| Session idle lifetime | 5 minutes | `SESSION_TTL_SECS` |
| Socket idle timeout | 45 seconds | `WS_IDLE_TIMEOUT_SECS` |
| Heartbeat ping interval | 15 seconds | `WS_HEARTBEAT_INTERVAL_SECS` |
| Progress throttle | 200 ms | `PROGRESS_INTERVAL_MS` |
| Server-wide buffered bytes | 200 MiB | `RELAY_BUDGET_BYTES` |

All values live in [`config.rs`](../src/config.rs).

The frame ceiling is deliberately above the recommended chunk size so a client
that pads or slightly overshoots is not disconnected.

Clients keep up to 16 MiB unacknowledged and acknowledge in 4 MiB batches. The
acknowledgement is what releases the sender's window, so a receiver's
acknowledgement interval must stay well below the sender's window or the
transfer stalls.

Buffered bytes are bounded across all sessions at once rather than per session.
Each chunk inside the relay holds a reservation against one ceiling, released
when the chunk reaches the receiver or when a session is discarded.

The socket survives a human-scale pause: the idle timeout is reset by the pong
answering each heartbeat ping. The five-minute session lifetime is the binding
constraint on an idle session, not the socket timeout.

## Planned changes

Not implemented. Tracked in
[`implementation-checklist.md`](implementation-checklist.md).

- **Receiver confirmation** adds `accept` and `decline` receiver messages and a
  `receiver_accepted` status, and moves the start trigger off
  `receiver_connected`.
- **End-to-end encryption** moves `filename` and `mime_type` out of cleartext
  `meta` into an encrypted blob, leaving only the ciphertext byte total visible
  to the relay, and adds a protocol version.
