# End-to-end encryption plan

Status: **proposed**
Created: **2026-08-19**
Last updated: **2026-08-19**

## Goal

The relay forwards bytes it cannot read. A Drop operator, and anyone who
compromises a Drop server, sees ciphertext and a byte count instead of files and
filenames.

## Context

This is the largest of the three planned changes and the only one that alters
what Drop *is*. It closes the gap the README currently states plainly: the relay
handles file bytes in memory while relaying them, so the operator and a
compromised server can access an active transfer.

It also requires a product decision, not just an implementation. AGENTS.md today
says: *"Do not describe Drop as peer-to-peer or end-to-end encrypted."* That rule
exists to stop the documentation overclaiming. Shipping this means deliberately
replacing it with a narrower, accurate claim. The pending decision is recorded
at [`../decisions.md`](../decisions.md) entry 7 and must be resolved there, not
inside an implementation branch.

### Why the architecture already suits it

- The relay never inspects chunk contents. `TransferService`
  ([`transfer_service.rs`](../../src/services/transfer_service.rs)) is under a
  hundred lines and forwards opaque binary frames against a byte budget.
  Encrypting the payload requires no change to the relay's hot path.
- The one thing the relay genuinely needs — the total byte count — survives
  encryption. An AEAD adds a fixed tag per chunk, so ciphertext length is a
  deterministic function of plaintext length. The sender can still declare an
  exact total up front, which is what the 4 GiB limit, progress reporting, and
  the compression design all depend on. See
  [`../decisions.md`](../decisions.md) entry 2.
- The same scheme lands in both clients, so browser-to-CLI transfers keep
  working.

### What the session code is today

Six uppercase hexadecimal characters from a UUIDv4
([`session_service.rs:68`](../../src/services/session_service.rs#L68)) — roughly
24 bits. [`../security.md`](../security.md) records this as a known weakness.
Encryption changes its meaning materially: once the key travels beside the code
rather than being implied by it, guessing a code no longer yields readable
bytes.

The key must be generated client-side and must never be derived from the code.
The relay knows the code.

## Constraints and invariants

- The relay must not be able to derive the key, and must never receive it.
- The sender must still declare an exact ciphertext length before sending.
- Chunk-level integrity must cover ordering and completeness, not just
  per-chunk contents. A malicious relay that reorders, drops, or truncates
  chunks must be detected.
- The 4 GiB limit, the relay budget, and the per-IP bounds all continue to
  apply against ciphertext.
- An old client meeting a new one must fail cleanly, never write garbage to
  disk.
- No claim may blur the browser case and the CLI case. See Risks.

## Non-goals

- Peer-to-peer transport. The relay stays in the path.
- Identity, authentication, or verifying *who* the other party is. This protects
  content from the relay; it does not tell either peer who they are talking to.
- Encrypting the byte count, session code, or connection metadata. The relay
  needs the first and sees the rest.
- Signed release artifacts. Related trust problem, separate work.

## Phases

### Phase 0 — Decide

- [ ] Resolve entry 7 in [`../decisions.md`](../decisions.md): whether Drop
      adopts this model, and exactly what claim replaces the AGENTS.md
      invariant.
- [ ] Choose the cipher — XChaCha20-Poly1305 or AES-GCM — and record why.
      Nonce handling differs materially between them; XChaCha's larger nonce is
      more forgiving of random generation.
- [ ] Choose the key encoding that travels beside the code, optimizing for
      something a person can paste and, ideally, read aloud.

### Phase 1 — Envelope

- [ ] Generate a random 256-bit key on the sending client.
- [ ] Define chunk framing: nonce derivation from a session-random prefix plus
      a counter, and a tag per chunk.
- [ ] Put the chunk counter and the total chunk count in the additional
      authenticated data, so reordering, dropping, or truncating is detected.
- [ ] Define the encrypted metadata blob carrying `filename` and `mime_type`.
- [ ] Add a protocol version to the cleartext envelope.

### Phase 2 — Relay

- [ ] Reduce cleartext `Meta` to the ciphertext byte total plus the version and
      the opaque metadata blob.
- [ ] Confirm the size limit, progress, and byte accounting all still work
      against ciphertext length.
- [ ] Confirm no plaintext filename can reach logs, tracing spans, or
      `/metrics`. `log_incoming_sender_message`
      ([`protocol.rs`](../../src/ws/protocol.rs)) currently logs the filename
      at debug.

### Phase 3 — Clients

- [ ] CLI: encrypt on send, decrypt on receive, carry the key as a suffix on
      the code argument.
- [ ] Web: same scheme via WebCrypto, with the key in the URL fragment, which
      browsers never send to the server.
- [ ] Make the failure modes clear: wrong key, tampered chunk, and version
      mismatch each need a distinct, comprehensible message.
- [ ] Confirm interoperability in both directions between the two clients.

### Phase 4 — Documentation

- [ ] Replace the AGENTS.md invariant with the precise claim agreed in Phase 0.
- [ ] Rewrite the README privacy section and
      [`../security.md`](../security.md), distinguishing the CLI case from the
      browser case explicitly.
- [ ] Update [`../protocol.md`](../protocol.md).
- [ ] Move the resolved decision into [`../decisions.md`](../decisions.md) and
      mark this plan done.

## Risks

- **Overclaiming.** This is the biggest risk and it is a documentation risk,
  not a code risk. Encryption in a browser is only as strong as the JavaScript
  the server delivered: it defeats a passive operator and stored traffic, but
  not a server that actively serves modified client code. The CLI-to-CLI case is
  the strong one. Any wording that implies browser transfers are as strong as
  CLI transfers is worse than making no claim at all.
- **Key handling in the shareable code.** The code plus key must stay pasteable
  and must not end up in a place that logs URLs. The URL fragment is not sent to
  the server, but it does land in browser history.
- **Nonce reuse** is catastrophic for both candidate ciphers. The counter scheme
  needs a test that a fresh session never reuses a nonce, and a design that
  cannot silently wrap.
- **Compression interaction.** Compress before encrypting, never after —
  encrypted bytes do not compress. The temporary-file length measurement in the
  CLI happens on the compressed plaintext, and the ciphertext length is then a
  deterministic function of it. Confirm the arithmetic rather than assuming it.
- **Silent downgrade.** A version field is only useful if a mismatch is a hard
  failure. Make sure an old client cannot be talked into a plaintext transfer by
  a hostile relay.
- **Partial-file damage.** A decryption failure at chunk N means N-1 chunks are
  already on disk. Decide whether the receiver truncates, removes, or reports
  the partial file, and do it deliberately.

## Validation

- [ ] A tampered chunk causes the receiver to fail rather than write corrupt
      output.
- [ ] Reordered and truncated chunk streams are both detected.
- [ ] A wrong key fails with a clear message, not a panic and not a partial
      file left silently in place.
- [ ] Declared ciphertext length matches bytes actually sent, for compressed
      and uncompressed payloads, across a range of sizes including one that is
      not a chunk multiple.
- [ ] A nonce is never reused within a session.
- [ ] Version mismatch fails cleanly in both directions.
- [ ] Browser-to-CLI and CLI-to-browser transfers interoperate.
- [ ] Relay logs, tracing output, and `/metrics` contain no plaintext filename.
- [ ] Full validation command set passes.

## Kickoff prompt

```text
Read docs/plans/end-to-end-encryption-plan-2026-08-19.md, docs/decisions.md,
docs/security.md, docs/protocol.md, and AGENTS.md. Phase 0 is a decision, not
code — do not start implementing until entry 7 in docs/decisions.md is
resolved and recorded. When implementing, the key is generated client-side and
never sent to the relay; the chunk counter and total chunk count go in the AEAD
additional data. Do not write any documentation claiming end-to-end encryption
without the browser caveat stated in the same place.
```

## Open questions

- XChaCha20-Poly1305 or AES-GCM? WebCrypto supports AES-GCM natively;
  XChaCha needs a bundled implementation in the browser but is more forgiving
  on nonces.
- What does the shareable string look like? `7F2A91#K3F9...` is one option;
  anything a person has to read aloud over a phone argues for a different
  alphabet.
- Should the browser support a receiver that has the code but not the key —
  failing with a helpful message rather than a decryption error?
- Does the metadata blob need padding? Filename length leaks a little
  information about the payload even when the name itself is hidden.
