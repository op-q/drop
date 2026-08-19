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

None. No implementation work has started.

## Proposed

- [`relay-teardown-drain-plan-2026-08-19.md`](relay-teardown-drain-plan-2026-08-19.md)
  — fix the intermittent `Connection reset by peer` a sender sees after a
  successful transfer. Pre-existing defect, deliberately deferred out of PR #30.
  Smallest of the three and a prerequisite in practice.
- [`receiver-confirmation-plan-2026-08-19.md`](receiver-confirmation-plan-2026-08-19.md)
  — show the receiver what it is about to accept and require a y/n before any
  bytes move. Adds a protocol handshake and a new terminal outcome.
- [`end-to-end-encryption-plan-2026-08-19.md`](end-to-end-encryption-plan-2026-08-19.md)
  — encrypt payloads client-side so the relay forwards bytes it cannot read.
  Largest, and it changes a stated product invariant.

## Done

None yet.

## Suggested order (dependencies, not law)

Teardown first. It is a real defect, it needs no protocol change, and the
confirmation feature adds a new terminal close path — decline — that would
inherit the same reset bug on the day it ships.

Confirmation second. It is reviewable without crypto, and it settles what the
receiver sees and when.

Encryption last. It relocates the very metadata fields the confirmation prompt
displays, so doing it after means the encryption change is a mechanical move of
settled fields rather than a simultaneous redesign of the consent flow and the
crypto envelope.

The one cost of this order: the confirmation prompt ships while the relay can
still see and forge the filename it displays. That is acceptable as an
intermediate state provided the documentation does not imply otherwise.
