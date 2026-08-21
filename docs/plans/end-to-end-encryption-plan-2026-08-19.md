# End-to-end encryption plan

Status: **active**
Created: **2026-08-19**
Last updated: **2026-08-21**

## Goal

Both peers derive a session key from the short transfer code without it ever
crossing the wire, and the payload is encrypted under that key. Any relay in
the path forwards bytes it cannot read.

## Phase 0 — Decided

Resolved 2026-08-20 and recorded in [`../decisions.md`](../decisions.md)
entry 7. Superseding the draft that lived here:

- **Model adopted.** Client-side encryption, relay sees ciphertext plus a byte
  total.
- **Cipher: AES-256-GCM**, not XChaCha20-Poly1305. The nonce-misuse headroom
  XChaCha buys is worth nothing when the key is freshly derived per transfer;
  AES-GCM is native in WebCrypto and hardware-accelerated at both ends.
- **Key agreement: SPAKE2**, seeded by the transfer code, with HKDF-SHA256 over
  its output. The key is never transmitted and never derived from anything the
  relay holds.
- **Claim:** CLI-to-CLI is end-to-end encrypted; the browser case is encrypted
  but bounded by the delivered JavaScript. Never blur the two.

The envelope is **transport-independent** by requirement, not by accident. The
same chunk format rides the peer-to-peer transport in
[`peer-to-peer-transport-plan-2026-08-20.md`](peer-to-peer-transport-plan-2026-08-20.md)
and the WebSocket relay. That is the property that turns the relay into an
untrusted fallback instead of a component to be deleted.

## Design

### The code

Four words from the BIP-39 English wordlist: `crossover-clockwork-ridge-atlas`.

- 2048^4 ≈ 44 bits. Well above the 24 bits of today's six hex characters, which
  matters more than it used to: [`../decisions.md`](../decisions.md) entry 10
  publishes a DHT record under a key derived from this code, so a guessable
  code leaks the sender's address even though the PAKE protects the bytes.
- BIP-39 is chosen for speakability and because every word has a unique 4-letter
  prefix, which makes prefix-completion on entry possible later.
- Case-insensitive on entry, and normalised before use as the PAKE password.
  This also fixes the recorded bug where `drop recv 4607f9` is rejected while
  `4607F9` succeeds.

### Handshake

`spake2` (0.4) symmetric mode over the Ed25519 group, password = the normalised
**words only**. The SPAKE2 identity is `drop/v1/transfer/<nameplate>`, which
binds the handshake to one session: a relay running two transfers cannot splice
a message from one into the other. Each side sends its PAKE message, receives
the peer's, and finishes to a shared secret. HKDF-SHA256 expands it with domain
separation:

| Info string | Output |
| --- | --- |
| `drop/v1/meta` | 32-byte key for the metadata blob |
| `drop/v1/chunk` | 32-byte key for payload chunks |
| `drop/v1/salt` | 4-byte per-session nonce prefix |

**One guess only.** A wrong code produces a different shared secret, so the
first authenticated frame fails to open. The session must then be destroyed
rather than retried — if a wrong guess can be retried on the same code, the
short code stops being safe. This is a protocol requirement, not an
implementation detail.

### Chunk framing

Nonce is 12 bytes: the 4-byte HKDF salt, then a `u64` big-endian counter
starting at zero and incrementing per chunk. At the 1 MiB chunk size and the
4 GiB limit the counter cannot exceed 4096, so it cannot wrap.

AAD per chunk binds position and completeness:

```text
AAD = version_u8 || chunk_index_u64_be || total_chunks_u64_be
```

Reordering changes `chunk_index`, truncation changes the count of chunks
actually delivered against the authenticated `total_chunks`, and dropping does
both. All three fail the tag rather than producing plausible output.

### Length accounting

`ciphertext_len = plaintext_len + 16 * ceil(plaintext_len / CHUNK)`.

Deterministic, so [`../decisions.md`](../decisions.md) entry 2 survives: the
sender still declares an exact total before the first byte. Compression happens
**before** encryption; the temporary-file measurement already yields the
compressed plaintext length, and the formula above converts it.

### Metadata

`filename`, `mime_type`, and the plaintext size move out of cleartext `meta`
into a blob sealed under the metadata key, nonce counter reserved at `u64::MAX`
so it can never collide with a chunk nonce. Cleartext `meta` retains only the
ciphertext byte total and the envelope version.

The blob's AAD binds the **sealed size**, not the chunk count. Binding the
chunk count is impossible: it is a function of the plaintext size, which is
inside the blob the receiver has not opened yet. The sealed size is known to
both ends beforehand, and binding it means a relay that rewrites the declared
size cannot have the metadata open either.

## Phases

### Phase 1 — Envelope, transport-independent — **done 2026-08-20**

- [x] New `crypto` module in the CLI library: wordlist codes, SPAKE2 handshake,
      HKDF derivation, sealing and opening chunks, metadata blob.
      `cli/src/crypto/{mod,code,handshake,envelope,wordlist}.rs`.
- [x] Pure and synchronous at its boundary — it takes and returns bytes, and
      knows nothing about sockets. This is what lets both transports share it.
- [x] Unit tests for tamper, reorder, truncate, wrong key, and length
      arithmetic including a payload that is not a chunk multiple.
      25 tests, all passing; workspace total 87 with nothing regressed.

Notes from building it:

- The BIP-39 wordlist is **vendored** into `wordlist.rs` rather than taken as a
  dependency. `bip39` pulls `bitcoin_hashes`, `serde`, and
  `unicode-normalization` to supply what is, for our purposes, a list of words,
  and `../decisions.md` entry 10 already flags dependency weight as a standing
  concern for a binary shipped prebuilt for four targets.
- Duplicate-chunk rejection came free from binding the index into the AAD, and
  is now covered by a test. It was not called out as a threat in the original
  plan; it should have been, since a relay can replay a frame as easily as it
  can drop one.
- `TransferCode`'s `Debug` is redacted. The type is a secret and the codebase
  logs liberally.
- Nonce-reuse coverage is structural rather than statistical: the test walks
  every nonce a maximum-size session can produce, including the metadata
  nonce, and asserts the set has no duplicates.

### Phase 2 — Relay path carries the envelope — **done 2026-08-21**

- [x] Reduce cleartext `Meta` to ciphertext total, version, opaque blob.
      `Session` no longer has a `filename` field at all.
- [x] Confirm the size limit, relay budget, and progress accounting all work
      against ciphertext length. Session creation now takes `ciphertext_size`.
- [x] Remove the filename from `log_incoming_sender_message` in
      [`protocol.rs`](../../src/ws/protocol.rs). The blob is not logged either:
      a ciphertext in a debug log is still an artefact of someone's transfer.
- [x] Carry the two PAKE messages through the relay as opaque frames.
- [x] Bound the opaque fields. Not in the original plan and it should have
      been: the relay cannot inspect these, so without a ceiling they are an
      unmetered side channel outside the transfer accounting entirely.

### Phase 3 — CLI — **done 2026-08-21**

Landed with Phase 2 rather than after it. The protocol change is breaking by
design — no downgrade path is permitted — so there was no green state with a
Phase 2 relay and a Phase 1 client.

- [x] Encrypt on send, decrypt on receive, over the relay transport.
- [x] Distinct, comprehensible failures for wrong code, tampered chunk, and
      version mismatch, via `CryptoError`.
- [x] Partial-file handling: `discard_partial` removes a partly written file on
      a decryption or truncation failure. An extraction directory is
      deliberately left alone — the entries written are individually authentic,
      and deleting a tree the receiver may already have had files in is worse
      than reporting the stop.

### Phase 4 — Web — **done**

Resolved by compiling the envelope rather than reimplementing it. `crypto/` is
now its own crate and `crypto-wasm/` builds it for the browser, so there is no
second implementation to keep byte-identical. The reasoning and its cost are in
[`../decisions.md`](../decisions.md) entry 11.

- [x] The whole envelope via WebAssembly, not just SPAKE2. WebCrypto would have
      covered AES-GCM and HKDF, but splitting the envelope across two
      implementations was the risk worth removing.
- [x] Interoperate both directions with the CLI over the relay.
      `web/tests/interop.test.mjs` runs a real relay and the real CLI binary.
- [x] A receiver holding a malformed code fails with a message that says so,
      before the relay is contacted.

### Phase 5 — Documentation

- [ ] Replace the AGENTS.md invariant with the claim agreed in Phase 0. Held
      until Phase 4: while the browser client cannot do a sealed transfer at
      all, a claim covering it would be false.
- [x] Rewrote the README privacy section and
      [`../security.md`](../security.md), CLI and browser cases separated.
- [x] Replaced the AGENTS.md invariant with the narrower claim from entry 7.
- [x] Update [`../protocol.md`](../protocol.md).

## Risks

- **Overclaiming.** The largest risk and it is a documentation risk. Browser
  encryption is bounded by the JavaScript the server delivered. Wording that
  implies browser transfers are as strong as CLI transfers is worse than
  claiming nothing.
- **Retry defeats the PAKE.** If a failed handshake leaves the code usable, an
  attacker gets unlimited guesses against 44 bits. The session must burn.
- **Nonce reuse** is catastrophic. The counter must be structurally incapable
  of wrapping and must never restart within a session — this constrains the
  resume design, which cannot simply reset the counter on reconnect.
- **Partial-file damage.** A failure at chunk N leaves N-1 chunks written.
- **Silent downgrade.** A version mismatch must be a hard failure, or a hostile
  relay talks an old client into a plaintext transfer.
- **Compression ordering.** Compress before encrypting, never after.

## Validation

- [x] Tampered, reordered, and truncated chunk streams are each detected.
      Covered at the envelope level, plus duplication. End-to-end evidence over
      a real transport is still owed by Phase 3.
- [x] A wrong code fails clearly, leaves no usable partial file, and burns the
      session so a second guess is impossible. Covered end to end by
      `a_wrong_code_is_refused_and_leaves_nothing_on_disk`. The burn is not new
      code: the relay already refuses a second receiver on a claimed session,
      so the first claimant spends it. That was luck rather than design, and is
      now pinned by a test.
- [x] Declared ciphertext length matches bytes sent, across sizes including a
      non-multiple of the chunk size. The compressed case is arithmetic on the
      same function and is confirmed once Phase 3 wires compression through.
- [x] A nonce is never reused within a session.
- [x] Version mismatch fails cleanly: the relay refuses a `meta` whose version
      it does not know, and the receiver refuses one it cannot speak.
      `envelope_version_matches_the_client` guards the duplicated constant.
- [x] Browser-to-CLI and CLI-to-browser interoperate, over a real relay with
      the real CLI binary, at sizes spanning a chunk boundary.
- [x] Relay logs, tracing, and `/metrics` contain no plaintext filename;
      checked against a live relay at `RUST_LOG=info` during interop.
- [x] Full validation command set passes: 100 Rust tests, fmt, clippy, the
      secret scan, the web build, `tsc --noEmit`, and 16 Node tests.

Not covered: `App.svelte` is checked by neither `tsc` nor a browser test. The
interop tests exercise the envelope and the wire protocol from Node. The Svelte
flows were changed to match and build clean, but "builds" is the evidence for
the UI layer, not "works".

## Open questions

- Does the metadata blob need padding? Filename length leaks a little even when
  the name is hidden.
- Should a receiver that has the code but meets a version it cannot speak be
  told to upgrade, naming the version it saw?
- Four words is a guess at the speakability/entropy balance. Three (33 bits) is
  meaningfully easier to read aloud and probably still enough given the PAKE;
  revisit once the DHT exposure in entry 10 is measured rather than assumed.
