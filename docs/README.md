# Documentation map

Docs are part of how Drop is built, not notes written afterwards. Keep every
document short, current, and tied to real behavior. When behavior changes,
change the code, the tests, and the relevant document together.

[AGENTS.md](../AGENTS.md) at the repository root is the canonical working guide
and takes precedence over anything here. The root [README](../README.md) is the
entry point for users and self-hosters. This file is the map of everything under
`docs/`.

## Start here

Read these in order when picking up the project:

1. [`architecture.md`](architecture.md) — what the pieces are, what runs
   where, and which direction the project is moving.
2. [`implementation-checklist.md`](implementation-checklist.md) — the tactical
   view: what is being built and what state it is in.
3. [`plans/README.md`](plans/README.md) — the plan contract and the full plans
   behind each item.
4. [`protocol.md`](protocol.md) — the wire contract both clients implement,
   including the envelope a second client would have to reproduce.
5. [`security.md`](security.md) — trust boundaries, hostile input, and the
   weaknesses that are known and accepted.
6. [`decisions.md`](decisions.md) — the choices that are expensive to reverse.

## Layout

```text
docs/
  plans/            full plans with checkbox progress, one file per topic
  validation/       repeatable test recipes and dated exploratory reports
  architecture.md   the crates, what runs where, and the direction of travel
  protocol.md       the HTTP and WebSocket contract, and the envelope
  security.md       trust boundaries, hostile input, resource bounds
  decisions.md      durable architectural decisions
  implementation-checklist.md   tactical status mirror
  release-checklist.md          evidence gate for tagging
  commands.md       short commands for humans and agents
```

## Document ownership

| Document | Owns |
| --- | --- |
| [`architecture.md`](architecture.md) | Orientation: the crates and how they depend on each other, where a transfer's work happens, the transport seam, and what the project is working towards |
| [`plans/`](plans/README.md) | Full plan context: phases, risks, validation, kickoff prompts, open questions |
| [`implementation-checklist.md`](implementation-checklist.md) | Honest status and work order, mirroring the plans |
| [`protocol.md`](protocol.md) | The session, control-message, framing, and envelope contract |
| [`security.md`](security.md) | Trust boundaries, hostile-input rules, resource bounds, known weaknesses |
| [`decisions.md`](decisions.md) | Architectural choices that are costly or confusing to reverse |
| [`release-checklist.md`](release-checklist.md) | Repeatable evidence gate for tagging a release |
| [`validation/`](validation/) | Exploratory test recipes and dated findings reports |
| [`commands.md`](commands.md) | The commands, including the ones easy to forget |
| [`../README.md`](../README.md) | Installation, CLI usage, configuration, deployment, operational limits |
| [`../AGENTS.md`](../AGENTS.md) | Working rules, product invariants, validation commands |
| [`../k8s/README.md`](../k8s/README.md) | Kubernetes manifests and the GKE overlay |

## Project policies

- [`../LICENSE`](../LICENSE) — MIT.
- [`../.github/CONTRIBUTING.md`](../.github/CONTRIBUTING.md) — branch workflow
  and pull request expectations.
- [`../.github/SECURITY.md`](../.github/SECURITY.md) — private vulnerability
  reporting.
- [`../.github/CODE_OF_CONDUCT.md`](../.github/CODE_OF_CONDUCT.md).

## Where information belongs

- User-facing CLI options, configuration variables, and deployment steps belong
  in the root README.
- Anything a third client would need to interoperate belongs in `protocol.md`.
- Attacker capabilities, hostile-input handling, and accepted risk belong in
  `security.md`.
- A choice someone would otherwise re-litigate belongs in `decisions.md`.
- The reasoning behind in-flight work belongs in a plan, not in a commit
  message and not only in this map.

Do not copy a detailed contract into more than one document. Summarize and link
to the owning document.

## Conventions

- Keep implemented, planned, deferred, and unsupported behavior visibly
  distinct. Drop is pre-release; never describe a plan as if it shipped.
- Save full plans in `plans/`, not in chat-only context, and mirror status into
  the checklist rather than duplicating the plan.
- Mark a checkbox complete only when its evidence exists. A check that was not
  run is reported as not run, never assumed to pass.
- Two claims are narrow and must stay narrow. **End-to-end encrypted** is true
  of CLI-to-CLI transfers and not of browser transfers, which are only as strong
  as the code the site delivered. **Peer-to-peer** is a claim about the direct
  QUIC path once it ships, never about Drop as a whole, since every transfer
  today crosses the relay. Never blur either pair. See
  [AGENTS.md](../AGENTS.md) and [`decisions.md`](decisions.md) entries 7 and 11.
- Record a decision entry when changing the persistence stance, the session
  lifecycle, the encryption model, the deployment shape, or the resource
  bounds.
- Use synthetic examples. Never put a real session code, transferred filename,
  client IP address, credential, or personal path in documentation.
- Add a document only when it owns a durable contract or a repeatable workflow.
  Do not create files for speculative components.
- Documents are kebab-case.

## Known cleanup

Tracked so it is not rediscovered, but left alone for now:

- `MAX_UPLOAD_SIZE_LABEL` in [`config.rs`](../src/config.rs) reads `4 GB` while
  the enforced limit is 4 GiB and every document says 4 GiB. It is user-visible
  in error text.
