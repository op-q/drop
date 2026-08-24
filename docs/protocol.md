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
{ "ciphertext_size": 432539152 }
```

Response:

```json
{ "code": "7F2A91" }
```

`ciphertext_size` is the sealed length, not the plaintext length. It is a
deterministic function of the plaintext length —
`plaintext + 16 * ceil(plaintext / 1 MiB)` — so the sender can still declare an
exact total before sending a byte. No filename is sent: it travels inside the
sealed metadata blob.

The relay rejects a zero `ciphertext_size`, one above the 4 GiB limit, a request
beyond the per-IP session-creation rate limit, a request once 100 concurrent
sessions exist, and any request while the process is draining for shutdown.

### Codes and the nameplate

The `code` returned here is the **nameplate**: six uppercase hexadecimal
characters. It is the only part of a transfer code the relay ever sees.

What a person is shown, and types on the other end, is longer:

```text
7F2A91-crossover-clockwork-ridge
^^^^^^ nameplate — sent to the relay, routes the two peers together
       ^^^^^^^^^^^^^^^^^^^^^^^^^ three words — never transmitted
```

The three words are the password for the key exchange below. They are drawn by
the sending client and never leave it. A relay that learned them could run the
exchange against both peers and read the transfer, which is why they are not
part of any message in this document. See [`security.md`](security.md).

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
both send key_exchange        -> relay forwards each to the other, opaquely
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

**A `key_exchange` is forwarded, not buffered.** The relay hands it to a peer
that is already connected and refuses one that arrives before the peer exists,
failing the session with `key exchange arrived before the sender connected` or
`key exchange arrived before the receiver connected`. Both halves are therefore
ordered:

- The **sender** waits for `receiver_connected` before sending its half.
- The **receiver** sends its half **in reply to the sender's**, never on
  connect. The sender's half arriving is what proves there is a peer to reply
  to.

Neither half may be sent on connect. Before the relay refused them, an early
half was dropped silently and the peer waited for a message that no longer
existed — the transfer stalled with no error on either side until the session
expired. That is what the refusal replaces, and it is why the receiver replies
rather than opens.

## Sender to relay

Text frames, tagged JSON:

| Message | Fields | Meaning |
| --- | --- | --- |
| `key_exchange` | `message` | One half of the SPAKE2 exchange, hex-encoded |
| `meta` | `version`, `ciphertext_size`, `metadata` | Describes the payload; must precede any binary frame |
| `complete` | — | All declared bytes have been sent |
| `cancel` | — | Abandon the transfer |

Binary frames carry sealed chunk bytes. A binary frame before `meta` is answered
with an error and ignored.

`meta.ciphertext_size` must equal the `ciphertext_size` given at session
creation. A mismatch fails the session. This is what stops a sender from
reserving a small session and then streaming something much larger.

`meta.version` must match the relay's envelope version or the session fails.
A version mismatch is deliberately fatal: a negotiated-down version is one a
hostile relay could steer toward plaintext.

`meta.metadata` is the sealed blob carrying `filename`, `mime_type`, and the
plaintext size, hex-encoded. The relay forwards it without being able to read
it. `metadata` and `key_exchange.message` are each capped at
`MAX_OPAQUE_FIELD_BYTES`; the relay cannot inspect them, so it bounds them
instead.

## Relay to sender

Text frames, tagged JSON:

| Message | Fields |
| --- | --- |
| `status` | `status` |
| `key_exchange` | `message` — the receiver's half, forwarded verbatim |
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
| `key_exchange` | `message` — the sender's half, forwarded verbatim |
| `meta` | `version`, `ciphertext_size`, `metadata` |
| `progress` | `bytes_transferred`, `total_bytes` |
| `complete` | — |
| `error` | `message` |

`meta` always precedes the first binary frame. A receiver that sees bytes first
should treat it as a protocol violation.

## Receiver to relay

Text frames, tagged JSON:

| Message | Fields | Meaning |
| --- | --- | --- |
| `key_exchange` | `message` | The receiver's half of the SPAKE2 exchange, hex-encoded |
| `chunk_ack` | `bytes_received` | Cumulative sealed bytes received |
| `complete` | `bytes_received` | Final byte count after closing the file |
| `error` | — | The receiver is abandoning the transfer |

`bytes_received` is cumulative, not per chunk, and is counted in **sealed**
bytes — the relay meters what crosses it, and what crosses it is ciphertext. A
receiver therefore tracks two totals: sealed bytes for acknowledgement, and
plaintext bytes for progress and for what it writes to disk. The relay only
reports success after the receiver's final count matches the declared size,
which is why a transfer is confirmed by the receiver rather than assumed by the
sender.

## Framing, flow control, and limits

| Property | Value | Source |
| --- | --- | --- |
| Recommended chunk size | 1 MiB plaintext | `RECOMMENDED_CHUNK_BYTES` |
| Sealed chunk overhead | 16 bytes per chunk | `TAG_BYTES` |
| Opaque field ceiling | 8 KiB | `MAX_OPAQUE_FIELD_BYTES` |
| Envelope version | 1 | `ENVELOPE_VERSION` |
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
- **A peer-to-peer QUIC transport** carrying this same envelope, with the relay
  as fallback. See
  [`plans/peer-to-peer-transport-plan-2026-08-20.md`](plans/peer-to-peer-transport-plan-2026-08-20.md).
