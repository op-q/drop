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

## The peer vocabulary, and what the relay adds to it

A transfer is a conversation between two peers. The relay carries it, and it
also embellishes it: it renames two frames and invents two more. Both are
listed here so a third client interoperates over either carrier, and so that
nothing below is mistaken for something a peer said.

| Frame | Who produces it | Meaning |
| --- | --- | --- |
| `key_exchange` | either peer | Forwarded verbatim |
| `meta` | the sender | Forwarded verbatim |
| binary chunks | the sender | Forwarded verbatim |
| `chunk_ack` | the receiver | The relay renames it `ack` for the sender |
| `complete` (sender) | the sender | All declared bytes are sent |
| `complete` (receiver, with `bytes_received`) | the receiver | The relay checks the count, then reports `status: transfer_complete` instead |
| `status: receiver_connected` | **the relay** | Invented. No peer sends it |
| `status: waiting_for_sender` / `waiting_for_receiver` | **the relay** | Invented |
| `status: sending` | **the relay** | Invented |
| `progress` | **the relay** | Invented; advisory, and safe to ignore |

A sender must accept the receiver's `chunk_ack` and `complete` as well as the
relay's `ack` and `status: transfer_complete`, because which one arrives says
only what carried the transfer. A sender that accepts a receiver's `complete`
must check `bytes_received` against what it sent: over the relay that check is
already done before the rewording, and skipping it directly would report
success on an unchecked claim.

A sender must **not** require `status: receiver_connected`. Whether the peer
has to be waited for at all is a property of the carrier, not of the protocol.
Recorded as entry 12 in [`decisions.md`](decisions.md).

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

## The envelope

What is inside the frames above. A relay never needs this section; a second
client implementation needs all of it.

### Key schedule

```text
code = NAMEPLATE-word-word-word
       ^^^^^^^^^ public, routes            ^^^^^^^^^^^^^^^ secret, authenticates

password = the three words, ASCII, dash-separated, lowercase
identity = "drop/v1/transfer/" + nameplate

SPAKE2 (Ed25519 group, symmetric mode, password, identity)
   └─ both peers exchange one message and agree on a shared secret

HKDF-SHA256 over that secret, no salt, three expansions:
   info "drop/v1/chunk" -> chunk_key  32 bytes
   info "drop/v1/meta"  -> meta_key   32 bytes
   info "drop/v1/salt"  -> salt        4 bytes
```

Symmetric mode is used because either peer may connect first and the protocol
has no natural A/B assignment. The identity binds a handshake to one session:
the nameplate is public so it adds no secrecy, but it stops a relay running two
sessions from splicing a message from one into the other.

Completing the handshake does **not** prove the peer knew the code. A wrong
password yields a well-formed message and a different key, which is detected
when the sealed metadata fails to open.

### Sealing

AES-256-GCM throughout. The 96-bit nonce is structural, never random:

```text
nonce = salt(4 bytes) || counter(8 bytes, big-endian)

  chunks    counter = 0, 1, 2, ... in order
  metadata  counter = 2^64 - 1        cannot collide with any chunk
```

Every sealed frame carries 17 bytes of additional authenticated data —
authenticated, not encrypted, and not transmitted, because both sides
reconstruct it:

```text
aad = version(1 byte) || index(8 bytes, BE) || total(8 bytes, BE)

  chunks    index = the chunk's counter,  total = total chunk count
  metadata  index = 2^64 - 1,             total = the declared ciphertext size
```

Binding every chunk to both its own index and the total count is what makes
four different attacks detectable:

| Attack | Caught by |
| --- | --- |
| Modified bytes | the GCM tag |
| Reordered chunks | the index in the AAD does not match the expected counter |
| Duplicated chunk | same |
| Truncated stream | the count of chunks opened against the authenticated `total` |

Truncation is the one a per-chunk tag cannot catch alone: every chunk that did
arrive is perfectly authentic. Only counting them against the total detects a
stream that simply stopped.

A nonce must never repeat under one key. The key here is freshly derived per
transfer and the counter cannot wrap within a transfer, so repetition is
impossible by construction rather than by luck — which is why a counter is used
instead of random nonces, and why a future resume feature cannot simply restart
the counter on reconnect.

### Sizes

| Value | Size |
| --- | --- |
| Plaintext chunk | 1 MiB, the last one shorter |
| Sealed chunk | plaintext + 16 tag bytes |
| Metadata plaintext | JSON of `{filename, mime_type, plaintext_size}` |
| Metadata on the wire | hex of the sealed blob, inside `meta.metadata` |

`meta.ciphertext_size` is the total sealed size and is what the relay meters,
bounds, and acknowledges. The plaintext size travels **inside** the sealed
metadata. Confusing the two stalls a transfer one authentication tag short of
finishing.

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

## The direct path: framing a transfer on a QUIC stream

Everything above describes a transfer carried by the relay. A CLI-to-CLI
transfer carries the **same envelope and the same vocabulary** over a QUIC
stream instead, and this section is only the difference.

Implemented in `cli/src/transport/framed.rs` and `cli/src/transport/quic.rs`.
Not yet reachable from the `drop` binary — see
[`plans/peer-to-peer-transport-plan-2026-08-20.md`](plans/peer-to-peer-transport-plan-2026-08-20.md)
phases 3 and 4.

### Why framing is needed at all

A WebSocket hands a transfer its framing for free: every message carries its own
length, and the text/binary opcode already says whether it is control or
payload. A QUIC stream gives neither — it is an ordered, reliable sequence of
bytes and nothing more. So the direct transport declares both.

```text
┌──────┬─────────────────┬────────────────┐
│ kind │ length (BE u32) │ payload        │
│ 1 B  │ 4 B             │ `length` bytes │
└──────┴─────────────────┴────────────────┘
  0x01  control — payload is UTF-8 JSON, the same objects as above
  0x02  chunk   — payload is sealed bytes, opaque to the transport
```

| Property | Value | Source |
| --- | --- | --- |
| ALPN | `drop/transfer/1` | `DROP_ALPN` |
| Header | 5 bytes | `HEADER_BYTES` |
| Maximum accepted frame | 1 MiB + 16 B | `MAX_FRAME_BYTES` |
| Streams per transfer | 1, bidirectional | — |

The length is declared *before* the payload, so a hostile peer can declare one
far larger than any real frame and make the reader allocate it. The cap is
checked before a single payload byte is read. It is the size of a sealed chunk —
plaintext plus tag — which covers control frames too, since the largest of those
carries hex-encoded sealed metadata the relay already bounds at 8 KiB.

The ALPN carries a version. Two builds whose framing disagrees are refused by
QUIC before either side allocates anything, which is cheaper and clearer than
discovering the mismatch in a frame header.

### Who opens, and who hangs up

Two orderings here are load-bearing, and both fail as hangs rather than errors
if reversed.

**The sender accepts the connection and opens the stream.** The receiver dials
and accepts it. This reads backwards until you know that `accept_bi` resolves
only when the peer first *writes*, not when it opens a stream — and Drop's
sender speaks first, since `key_exchange` originates there. The receiver is the
one that resolved an address, so it is necessarily the dialler.

**Only the sender closes the connection.** A QUIC `CONNECTION_CLOSE` permits the
peer to discard stream data it has received but not yet handed to its
application, *including data it already acknowledged*. The receiver's final act
is writing the `complete` that the sender is blocked reading, so a receiver that
closes immediately destroys that frame in flight — and the sender reports a lost
connection for a transfer whose file is already correct on disk. The rule, as
iroh states it on `Connection::close`: only the peer last **receiving**
application data can be certain everything arrived, and closing is then the only
reliable thing it can do. In Drop that peer is the sender. The receiver finishes
its stream, which flushes and signals that no more frames are coming, and waits
to be closed.

### What is unchanged

- **The envelope.** Same key schedule, same nonces, same AAD, same sizes. A
  transport that could tell a sealed chunk from any other bytes would be a
  transport that could read the payload.
- **The control vocabulary.** The peer's words are canonical and the relay's are
  the embellishment, which is exactly what makes one vocabulary serve both
  carriers. See the section above and
  [`decisions.md`](decisions.md) entry 12.
- **The flow control.** Same 16 MiB window, same 4 MiB acknowledgement batches.
  QUIC has flow control of its own underneath, but the application-level window
  is what bounds the receiver's write-out, not the network.

## Planned changes

Not implemented. Tracked in
[`implementation-checklist.md`](implementation-checklist.md).

- **Receiver confirmation** adds `accept` and `decline` receiver messages and a
  `receiver_accepted` status, and moves the start trigger off
  `receiver_connected`.
- **Rendezvous and fallback for the direct path.** The transport above exists
  and carries whole transfers in test, but nothing in the `drop` binary reaches
  it: two peers still have no way to find each other without the relay, and
  there is no selection between the paths. See
  [`plans/peer-to-peer-transport-plan-2026-08-20.md`](plans/peer-to-peer-transport-plan-2026-08-20.md)
  phases 3 and 4, and the unsolved one-guess question it records.
