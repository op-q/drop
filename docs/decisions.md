# Decisions

Choices that are expensive or confusing to reverse, and the reasoning behind
them. Add an entry when changing the persistence stance, the session lifecycle,
the encryption model, the deployment shape, or the resource bounds.

Each entry records what was decided and what it costs. A decision that turns out
to be wrong is superseded by a new entry rather than edited into agreement with
the present.

## 1. The relay never persists file bytes

**Decision.** File bytes exist only in memory while being relayed between two
connected peers. The server writes no application storage.

**Why.** It is the product. An ephemeral relay has a much smaller surface than a
service that stores files: no deletion policy, no retention question, no backup
containing user data, no storage bill.

**Consequences.** Both peers must be online simultaneously. Resume after a
disconnect cannot be solved server-side. Adding persistence requires an explicit
product and threat-model decision, not an incremental commit.

The CLI may write a local temporary file to compress a payload, and must delete
it on every exit path including interruption. That is a client-side exception,
not a relaxation of the server rule.

## 2. The exact payload length is declared before the first byte

**Decision.** A sender commits to a total byte count at session creation, and
`meta` must match it.

**Why.** It lets the relay enforce the size limit before buffering anything,
gives both ends real progress and ETA, and lets the receiver detect a truncated
transfer instead of writing a short file and calling it done.

**Consequences.** Compression cannot be streamed. A compressed payload is
written to a temporary file first to learn its length, which costs local disk
and a second pass — worth it for source trees and documents, wasted on media
that is already compressed. This is why compression is off by default.

A file that changes size while being read is padded or truncated to the length
recorded at scan time, with a warning, because the declared total is already
committed.

This constraint survives encryption: an AEAD adds a fixed tag per chunk, so
ciphertext length stays a deterministic function of plaintext length.

## 3. Buffered bytes are bounded server-wide, not per session

**Decision.** One 200 MiB ceiling covers relayed file data across all sessions.
Each buffered chunk holds a reservation against it.

**Why.** A per-session bound multiplies out to sessions × capacity × chunk size,
which cannot grow with the chunk size and stay inside a container memory limit.
A shared budget decouples the two: one transfer may use a large window, while a
hundred concurrent transfers share the ceiling instead of multiplying it.

**Consequences.** Throughput on a busy relay depends on total load, not only on
one transfer's window. A reservation is returned when its chunk reaches the
receiver and when a session is discarded, so an abandoned transfer cannot
strand capacity.

## 4. One process, one replica

**Decision.** Sessions and live transfer channels exist only in process memory.
The Kubernetes Deployment runs exactly one pod with a `Recreate` strategy.

**Why.** Session state is a live pair of WebSockets and bounded channels, not a
row. Two replicas would split a sender and receiver across processes that cannot
see each other. `Recreate` avoids that during a rollout.

**Consequences.** No horizontal scaling. A rollout interrupts active transfers,
which is why the server drains on `SIGTERM` and reports readiness separately
from liveness. Scaling out later needs shared session coordination and
transfer-aware routing; session affinity alone cannot recover a live WebSocket.

## 5. One sender and one receiver per session

**Decision.** A code admits exactly one of each. Extra connections are refused.

**Why.** It makes the code a single-use capability rather than a channel anyone
can watch, and it makes a stolen code detectable: the legitimate receiver is
refused rather than silently sharing the stream.

**Consequences.** No multi-receiver fan-out. Rejoining after a dropped
connection is not possible, because the slot is consumed.

## 6. A folder is sent as one tar archive

**Decision.** The CLI archives a directory into a single tar stream rather than
transferring files individually.

**Why.** One declared length, one progress bar, one acknowledgement window. Per
file transfers would need their own framing and would make the declared-length
decision above much harder.

**Consequences.** The receiver must treat the archive as hostile input, which is
where the extraction rules in [`security.md`](security.md) come from. Sockets,
FIFOs, and device nodes are skipped. The whole transfer fails as a unit.

## 7. End-to-end encryption (pending)

**Status.** Not decided. Recorded here because it changes a stated invariant and
should not be made incidentally inside an implementation branch.

**What is being decided.** Whether Drop encrypts payloads client-side so the
relay forwards bytes it cannot read, with the key carried beside the session
code rather than inside it.

**What it changes.** AGENTS.md currently forbids describing Drop as end-to-end
encrypted. That rule exists to stop the documentation overclaiming, and shipping
this means deliberately replacing it with a precise claim rather than deleting
it.

**The claim has to stay honest.** Encryption in a browser is only as strong as
the JavaScript the server delivered: it defeats a passive operator and stored
traffic, but not a server that actively serves modified client code. The
CLI-to-CLI case is the strong one. Any wording that blurs the two is worse than
no claim.

Record the outcome here when it is made.
