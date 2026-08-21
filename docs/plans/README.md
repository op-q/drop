# Plans

Full plans with checkbox progress. These files preserve the entire plan
context — phases, file lists, risks, validation steps, kickoff prompts, and
open questions — so work can resume on another machine with no chat history.

The binding working rules live in [AGENTS.md](../../AGENTS.md). This file makes
the plan contract unmissable and indexes what is here.

## The plan contract

1. **Write plans in full.** When a multi-step plan is created, save the entire
   plan as `docs/plans/<topic>-plan-YYYY-MM-DD.md` before or as implementation
   starts. Never compress a plan to a summary. If a decision took investigation
   to reach, the investigation belongs in the plan, not in the commit message.
2. **Update checkpoints while working.** Flip `[ ]` to `[x]` as work lands, add
   short inline notes for blockers or scope changes, and update the
   `Last updated:` line on material changes.
3. **Mirror, do not duplicate.** Mirror status into
   [`../implementation-checklist.md`](../implementation-checklist.md), which
   stays the tactical view. The plan is the detailed source of truth.
4. **Verify before building.** A plan records intent at writing time; the
   repository may have moved. Check a plan's context claims — especially file
   and line references — against the source before building on them, and fix
   the plan if it drifted.
5. **Status header.** Every plan carries
   `Status: proposed | active | done | abandoned`. Mark an abandoned plan
   rather than deleting it, with one line saying why.
6. **Promote durable decisions.** When a plan settles something costly to
   reverse, move it into [`../decisions.md`](../decisions.md) and mark the plan
   done. Plans are working documents; decisions outlive them.

## Active

- [`end-to-end-encryption-plan-2026-08-19.md`](end-to-end-encryption-plan-2026-08-19.md)
  — Phase 0 resolved 2026-08-20: AES-256-GCM under a key derived from the code
  by SPAKE2, in a transport-independent envelope. Implementation started at
  Phase 1.

## Proposed

- [`peer-to-peer-transport-plan-2026-08-20.md`](peer-to-peer-transport-plan-2026-08-20.md)
  — connect the two CLIs directly over QUIC with `iroh`, finding each other
  through a mainline-DHT record derived from the code, so a transfer needs no
  Drop-operated server. Depends on the encryption envelope landing first.
- [`receiver-confirmation-plan-2026-08-19.md`](receiver-confirmation-plan-2026-08-19.md)
  — show the receiver what it is about to accept and require a y/n before any
  bytes move. Adds a protocol handshake and a new terminal outcome.

## Done

- [`relay-teardown-drain-plan-2026-08-19.md`](relay-teardown-drain-plan-2026-08-19.md)
  — completed 2026-08-19: the upload receive task now stays alive through
  teardown and answers the sender's closing handshake, so a socket is no longer
  dropped with the peer's reply unread. 80 runs clean against 3 failures in 30
  before. Two findings recorded rather than fixed: the assertions #30 removed
  stay out because they guard nothing under per-transfer relay isolation, and
  the download socket carries the same latent shape without a user-visible
  symptom.

## Suggested order (dependencies, not law)

Teardown first — **done**. It was a real defect, it needed no protocol change,
and the confirmation feature adds a new terminal close path, decline, that
would have inherited the same reset bug on the day it shipped.

Confirmation next. It is reviewable without crypto, and it settles what the
receiver sees and when.

**Revised 2026-08-20.** Encryption was moved ahead of confirmation at the
user's direction, and peer-to-peer transport was added after it. The order is
now: encryption, then transport, then confirmation.

The cost of the swap is real and was accepted knowingly. Confirmation would
have settled what the receiver sees before encryption relocated those fields;
doing encryption first means the confirmation prompt must be designed against
metadata that is already sealed, and its plan will need revisiting rather than
implementing as written.

Transport follows encryption because the envelope is what makes a relay
untrusted, and an untrusted relay is what makes falling back to one acceptable.
Building the QUIC path first would have produced a fast path with no honest
story for the slow one.

Confirmation last is otherwise unchanged, and no longer carries the caveat that
it ships while the relay can forge the filename it displays.
