# `meta_ok` key confirmation plan

Status: **proposed**
Created: **2026-08-31**
Last updated: **2026-08-31**

## Goal

Make the receiver *prove* it opened the sealed metadata instead of merely
saying so. The `meta_ok` frame gains a key-confirmation value derived from the
agreed secret, and the sender streams no payload byte until that value matches
its own copy.

This closes a gap in decision 13, not in the encryption. No payload was ever
readable by a wrong guesser. What is at stake is whether the human is told.

## Context

Decision 13 settled that the sender limits guessing on the direct path and asks
a human before granting a second attempt, because a 33-bit code is only
defensible under a single try and a serverless transfer has no relay to refuse
a second claim.

### What already exists

Verified against `feat/transport-selection` on 2026-08-31:

- The receiver announces it opened the metadata:
  [`recv.rs:228`](../../cli/src/recv.rs#L228) sends
  `{"type": "meta_ok"}` — and only when
  `transport.peers_enforce_one_guess()`.
- The sender blocks on it before streaming:
  [`send.rs:290`](../../cli/src/send.rs#L290) gates on the same method and
  calls `await_meta_checkpoint`.
- The checkpoint itself is
  [`send.rs:483`](../../cli/src/send.rs#L483). It reads **exactly one frame**
  under `META_CHECKPOINT_TIMEOUT`, and maps a timeout, a disconnect, an
  `error`, a chunk, and any other control frame onto one outcome:
  `Attempt::FailedTheCode`.
- Which carrier polices guessing is a trait method with **no default**:
  [`transport/mod.rs:94`](../../cli/src/transport/mod.rs#L94). A new carrier
  must answer it or fail to compile.
- The endpoint already accepts more than once:
  [`quic.rs:145`](../../cli/src/transport/quic.rs#L145) takes `&self`.
- The key schedule derives three secrets from the agreed secret via HKDF:
  [`handshake.rs:35-37`](../../crypto/src/handshake.rs#L35-L37).

### The gap

`meta_ok` is an unauthenticated assertion, and the party making it is the
party being rate-limited. An attacker who guessed the code wrong cannot open
the metadata — but nothing stops it sending `meta_ok` anyway.

| | Sender records | Human sees |
| --- | --- | --- |
| Honest mistype, self-reported | a failed attempt | the prompt |
| **Attacker, self-reported** | **a normal transfer** | **nothing** |
| Attacker, against a confirmation | a failed attempt | the prompt |

The rate limit survives either way: the transfer is consumed and the attacker
still got one guess. What does not survive is the property decision 13 chose
the prompt *for* — that being attacked is something the victim notices. An
attacker that never admits failing never triggers the prompt, the attempt
counter never climbs, and the sender reports an ordinary completed transfer to
a peer that could not read a byte of it.

The mechanism would look correct and quietly do nothing. That is the same
failure decision 13 was itself written to catch one level down.

## Design

A fourth HKDF output alongside the chunk key, metadata key and salt:

```text
sender                                  receiver
  meta (sealed) ------------------------>
                                          open_metadata()
       <---------------------------- meta_ok { confirmation }
  keys.confirms(confirmation)?
  chunk ------------------------------->
```

- Info string `drop/v1/confirm`, 32 bytes, same `Hkdf::<Sha256>` expansion as
  the existing three.
- The receiver sends it hex-encoded on the existing `meta_ok` frame. No new
  frame type, so the control vocabulary is unchanged.
- The sender compares in **constant time** (`subtle::ConstantTimeEq`). An
  early-returning comparison leaks how many leading bytes matched, which lets
  an attacker forge the value byte by byte — reopening the oracle from the
  other end. Length is checked first and is not secret.
- A malformed, missing, non-hex or non-matching value joins the existing
  outcomes in `await_meta_checkpoint`: one consumed attempt, one prompt.

Sending it reveals nothing. It is one non-invertible HKDF output away from the
secret, it authenticates only a peer that already holds the keys, and the
sender never transmits its own copy, so there is no value in the transcript to
replay back.

### Why a derived value rather than a re-encryption

The alternative was to have the receiver seal something under the metadata key
and let the sender open it. That also proves possession, but it puts a second
sealing context on the wire, and every additional use of a key is another
chance to reuse a nonce. One HKDF output costs nothing and reveals no more
than the metadata blob already does.

### Reference implementation

A spike exists on the local-only branch `feat/sender-enforces-one-guess`
(commit `ff4e9a1`). It is **not pushed and should not be merged** — see
*Already superseded* below. The part worth reusing:

```rust
const CONFIRM_INFO: &[u8] = b"drop/v1/confirm";
pub const CONFIRMATION_BYTES: usize = 32;

impl SessionKeys {
    /// Proof, for the peer, that this side derived the same keys.
    pub fn confirmation(&self) -> [u8; CONFIRMATION_BYTES] { self.confirmation }

    /// Whether `offered` is the confirmation these keys expect. Constant time.
    pub fn confirms(&self, offered: &[u8]) -> bool {
        // `ct_eq` is only constant time across equal lengths, so the length
        // check has to come first and is not itself secret.
        offered.len() == CONFIRMATION_BYTES && self.confirmation.ct_eq(offered).into()
    }
}
```

## Phases

### Phase 1 — key schedule

- [ ] Add `subtle = "2"` to `crypto/Cargo.toml`.
- [ ] Add `CONFIRM_INFO`, `CONFIRMATION_BYTES`, the `confirmation` field, and
      the fourth `expand` call in `crypto/src/handshake.rs`.
- [ ] Export `CONFIRMATION_BYTES` from `crypto/src/lib.rs`.
- [ ] Tests: matching codes agree on the confirmation; it differs from the
      chunk key, the metadata key and the salt; mismatched codes disagree.

### Phase 2 — the wire

- [ ] `recv.rs`: attach the hex confirmation to the existing `meta_ok`.
- [ ] `send.rs`: in `await_meta_checkpoint`, decode and compare, folding every
      failure into the existing `FailedTheCode` outcome.
- [ ] Tests, driven through `ScriptedTransport`: a real SPAKE2 pair confirms;
      a wrong-code peer is refused; non-hex is refused; absent is refused;
      data-before-confirming is refused.

### Phase 3 — the browser

- [ ] Confirm no change is needed. The browser is a relay client, the relay
      answers `peers_enforce_one_guess()` as false, and the WebAssembly build
      shares `crypto/`, so the fourth output appears automatically and is
      simply unused.

### Phase 4 — documentation

- [ ] `docs/protocol.md`: the key schedule gains a line; `meta_ok` gains a
      field, direct path only.
- [ ] `docs/security.md`: state what the confirmation does and does not buy.
- [ ] `docs/decisions.md`: promote to entry 15 once this lands (contract rule
      6), and mark this plan done.
- [ ] Mirror into `docs/implementation-checklist.md`.

## Files

| File | Change |
| --- | --- |
| `crypto/Cargo.toml` | add `subtle` |
| `crypto/src/handshake.rs` | fourth derived output, `confirmation`, `confirms` |
| `crypto/src/lib.rs` | export `CONFIRMATION_BYTES` |
| `cli/src/recv.rs` | attach confirmation to `meta_ok` |
| `cli/src/send.rs` | verify it in `await_meta_checkpoint` |
| `docs/protocol.md`, `docs/security.md`, `docs/decisions.md` | record it |

## Risks

- **Wire compatibility.** A new-sender/old-receiver pair on the direct path
  fails: the receiver sends a bare `meta_ok` and the sender charges an
  attempt. The direct path is unreleased, so this is acceptable now and will
  not be later. Land it before the QUIC path ships.
- **The relay path must not change.** The relay parses receiver frames into a
  closed set and treats an unknown one as fatal. The confirmation must ride
  only where `peers_enforce_one_guess()` is true. Decision 13's commit records
  that a shape which always sent the frame nearly shipped and would have
  broken production.
- **Constant-time comparison is easy to lose.** A later refactor to `==` would
  be silent. The comparison belongs in `crypto/`, never inlined at the call
  site.
- **One extra round trip** before the first byte, on the direct path only.
  Accepted: the payload is what is being protected.

## Validation

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace --all-targets`
- [ ] `npm --prefix web run build && npm --prefix web test` — the WebAssembly
      build shares `crypto/`, so it must still compile.
- [ ] `scripts/check-secrets.sh`
- [ ] A wrong-code peer against a real SPAKE2 exchange is refused, and the
      human is prompted.

## Already superseded — do not reimplement

The spike that produced this finding was built on a base predating the
decision-13 merge. Most of it is dead, and re-applying it would be a
regression:

- `limits_guessing()` **defaulting to `false`** was the spike's trait method.
  Main shipped `peers_enforce_one_guess()` with **no default** so a carrier
  that has not thought about it fails to compile. Main's is stricter; keep it.
  The two also carry opposite polarity, so they are not interchangeable.
- A `PeerSource` trait existed to let the sender accept a second peer, because
  the spike's `accept_transfer` consumed the endpoint. Main already takes
  `&self`. Nothing left to solve.
- The spike also predated work in `transport/framed.rs`, the release workflow,
  and several dependency bumps.

Only the key schedule and the `meta_ok` field are new work.

## Open questions

- Should a failed confirmation be distinguishable in the prompt from a peer
  that reported `error`? Decision 13 deliberately collapses outcomes so an
  honest mistyper and a silent attacker look alike. The same argument probably
  applies here, but it is worth stating rather than inheriting.
- Does the receiver deserve a symmetric proof that the *sender* held the keys?
  Out of scope: a sender that did not hold them could not have sealed the
  metadata the receiver just opened.
