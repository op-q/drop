# Self-hosted rendezvous plan

Status: **in progress** — phase 1 outstanding
Created: **2026-09-10**
Last updated: **2026-09-10**

## Goal

Let a Drop deployment point the direct path at **its own** rendezvous
infrastructure — an iroh relay it runs, a DHT it runs — instead of the public
defaults compiled in today.

Two values, read from the environment, defaulting to exactly what ships now:

```text
DROP_RENDEZVOUS_RELAY      http://relay.example.internal:3340
DROP_RENDEZVOUS_BOOTSTRAP  10.0.0.9:6881,10.0.0.10:6881
```

Unset, `drop` behaves as it does today: n0's relays and the mainline DHT's
public routers.

## Why this is a feature and not a test's convenience

It is worth being direct about the provenance, because the rule it brushes
against is a good one. This is
[open question 1](network-lab-plan-2026-08-31.md#open-question-1--how-the-direct-path-becomes-testable)
of the network lab plan, answered as option B, and that plan's own constraints
say **"no runtime configuration is added for a test's convenience."** A reader
who finds this plan through that one is entitled to ask whether the rule just
got bent for a test directory.

It did not, and the reason is that the gap exists without the lab:

- A self-hoster can already run their own Drop relay — that is what
  `--server` is for, and [`../decisions.md`](../decisions.md) entry 8 describes
  the split deployment it serves.
- They **cannot** run their own rendezvous. `RelayMode::Default` and pkarr's
  default bootstrap are compiled in, so the direct path reaches n0 and the
  public DHT or it does not happen.
- So inside an air-gapped or egress-filtered network — the exact kind of place
  that self-hosts a relay — the direct path cannot be used at all. Every
  transfer falls back, and `--transport p2p` fails outright.

That is a real deployment gap in a shipped feature. Closing it makes the lab
possible as a **side effect**, and the test would not have been a sufficient
reason on its own.

What would have been test convenience, and is not being done: a flag that
weakens [`../decisions.md`](../decisions.md) entry 14's address filter so a
private address can be published. See [What does not change](#what-does-not-change).

## What changes

| Where | Today | After |
| --- | --- | --- |
| [`../../cli/src/transport/quic.rs`](../../cli/src/transport/quic.rs) | `bind()` hard-codes `RelayMode::Default` | binds with a `RelayMode` the caller supplies |
| [`../../cli/src/transport/rendezvous.rs`](../../cli/src/transport/rendezvous.rs) | `MainlineDirectory::new()` takes pkarr's default bootstrap | takes the bootstrap nodes it is given, empty meaning default |
| [`../../cli/src/direct.rs`](../../cli/src/direct.rs) | assembles the two with no configuration between them | owns a `Rendezvous` read once from the environment, and hands each half what it needs |
| [`../../cli/src/main.rs`](../../cli/src/main.rs) | no mention | an `ENVIRONMENT` block in the help |

`direct.rs` is the right home for `Rendezvous` because it is already the
assembly point — its own doc says it exists so "a caller assembling a
serverless transfer names one module rather than three", and this is a third
thing to name.

### Environment variables and not flags

`--server` is per transfer: a user may reasonably send one file through a
different relay. Rendezvous infrastructure is not like that. It is a property
of the network the machine is on, set once in a shell profile, a systemd unit
or a container spec, and changing it between two transfers from the same
machine is not a thing anyone wants to do.

So these are documented in the help but have no flags. Two more flags on a
nine-option CLI, for something almost nobody sets and nobody sets twice, buys
discoverability that an `ENVIRONMENT` block already provides and charges every
reader of `--help` for it.

The asymmetry with `--status` is deliberate and runs the other way: that one has
a flag *and* a variable because a harness wants the variable and a person
debugging one transfer wants the flag.

## What does not change

- **Entry 14's filter.** `publishable` still strips every private address from
  every record. A lab-local relay works *with* the filter rather than around
  it: `is_publishable` accepts `TransportAddr::Relay(_)` unconditionally, so an
  endpoint whose only surviving address is a self-hosted relay URL still
  produces a valid record. Nothing here makes a private address publishable,
  and a change that did would need its own decision and would not get one.
- **The default.** Unset means n0 and the mainline DHT. No deployment that does
  not ask for this sees any difference, and the record format is untouched.
- **The mainline DHT as the store.** Entry 10 chose it; this chooses which
  nodes bootstrap into it, which is a different question.
- **The Drop relay.** `--server` and the relay fallback are not involved. A
  custom rendezvous changes who introduces two peers, never who carries bytes.
- **The browser.** It has no direct path, so it has no rendezvous.
- **One-guess enforcement, the envelope, the framing.** Untouched.

## What an operator takes on

Worth stating plainly, because this hands someone a way to make their own
transfers worse.

A relay named here sees **connection metadata** for every direct transfer that
uses it: which endpoint ids talk to each other, when, and their observed
addresses. It does not see file bytes, filenames or the code — those are sealed
by the envelope, and `--transport p2p` does not weaken that. So a hostile relay
URL is a privacy problem and not a confidentiality one, and it is the same
exposure n0's relays have today, moved to a host the operator chose.

A bootstrap node named here can **lie by omission**: refuse to store a record,
or serve a stale one. The consequence is a failed rendezvous, which falls back
to the relay. It cannot forge a record that resolves to a peer the receiver
will then trust, because trust does not come from the record —
`rendezvous.rs`'s own note says a resolved record "is **not proof of who
published it**", and authentication is SPAKE2's.

So the failure modes are degraded privacy and failed setup, never a transfer to
the wrong party. That is what makes an environment variable an acceptable
interface for it.

## Phases

### Phase 1 — The two knobs

- [ ] `Rendezvous` in `direct.rs`: parsed from `DROP_RENDEZVOUS_RELAY` and
      `DROP_RENDEZVOUS_BOOTSTRAP`, with a malformed value **failing loudly**
      rather than falling back to the default. Silently ignoring a typo in a
      relay URL would send an operator's traffic to n0 while they believed it
      was staying inside their network, which is the one outcome worse than an
      error.
- [ ] `QuicEndpoint::bind` takes a `RelayMode`; `bind_without_relays` stays as
      the name for the LAN-only case so existing tests keep reading correctly.
- [ ] `MainlineDirectory::new` takes bootstrap nodes.
- [ ] `publish_sender` and `dial_sender` take the `Rendezvous` they bind with.
- [ ] Unit tests for parsing: unset, set, a malformed URL, a malformed
      bootstrap entry, and that an empty variable is treated as unset rather
      than as "no bootstrap nodes at all" — the second would silently disable
      the DHT.
- [ ] `ENVIRONMENT` block in `USAGE`, naming both and saying what unset means.
- [ ] `security.md`: what an operator takes on, from the section above.

Deliberately **not** in phase 1: a pkarr relay. pkarr's HTTP relay client is
excluded today by `default-features = false`, which `cli/Cargo.toml` says is
"the only thing pulling in reqwest". Re-admitting reqwest into a binary that
ships prebuilt for four targets is a dependency decision of its own, and the
DHT bootstrap knob covers the same ground without it.

### Phase 2 — Consumed by the lab

Not work in this plan, and listed so the dependency is visible: Phase 4 of
[`network-lab-plan-2026-08-31.md`](network-lab-plan-2026-08-31.md) runs an iroh
relay and a mainline testnet in a namespace and points both variables at them.
That is where this gets exercised end to end.

## Risks

- **An operator points this at something hostile.** Addressed above: the
  exposure is metadata, and it is the exposure n0 already has. Stated in
  `security.md` rather than guarded against in code, because a machine cannot
  tell a trusted relay from an untrusted one.
- **A typo silently downgrades privacy.** This is the sharp one, and it is why
  a malformed value is an error. An operator who meant to keep rendezvous
  inside their network and instead published to the public DHT has lost exactly
  what they configured this to protect, and would have no way to notice.
- **Hole punching needs address discovery, which needs TLS.** Resolved during
  implementation rather than left open: behind NAT at both ends a peer's only
  candidates are private addresses, so without discovery no translated address
  is ever learned and no punch is attempted. The lab's helper therefore serves
  QUIC address discovery on a self-signed certificate, which iroh's client
  accepts because it installs a custom verifier — it authenticates by endpoint
  id, not by certificate chain. The port is not configurable: `RelayMode::custom`
  gives every entry `RelayQuicConfig::default()`, so the client probes 7842 and
  nowhere else. Worth knowing if a second relay URL is ever added.
- **Scope creep into a discovery framework.** Two values, read once, with no
  precedence rules, no config file, and no per-peer overrides. A third value
  should be suspected of being a different feature.

## Validation

- [ ] `cargo test --workspace --all-targets`, `cargo fmt --all -- --check`, and
      `cargo clippy --workspace --all-targets --all-features -- -D warnings`
      clean.
- [ ] Unset variables produce byte-identical behaviour to `main`: the default
      relay mode and the default bootstrap, asserted rather than assumed.
- [ ] A malformed value fails with a message naming the variable.
- [ ] Two `drop` processes complete a direct transfer against a relay and a DHT
      on **loopback**, with no public network reachable. This is the end-to-end
      gate, and it runs before any namespace work — loopback cannot prove
      anything about NAT, but it proves both knobs are wired to something real.
- [ ] `scripts/check-secrets.sh` clean.
- [ ] [`../implementation-checklist.md`](../implementation-checklist.md)
      mirrored in the same change.

## Open questions

- ~~Does hole punching work against a plain-HTTP relay with no QUIC address
  discovery?~~ **Answered while implementing: no, and it is not close.** Behind
  NAT at both ends the only candidate addresses are private, so with no
  discovery neither peer learns a translated address and the two NAT topologies
  would have been indistinguishable — both relayed — with the lab measuring its
  own omission. The lab's helper serves discovery on a self-signed certificate;
  see the risk above. The remaining unknown is empirical rather than
  structural: whether a punch through `nf_conntrack` succeeds in practice, which
  the full-cone topology reports either way.
- **Should a relay URL be allowed to be a bare host?** `http://host:3340` is
  what iroh wants. Accepting `host:3340` and inferring a scheme is friendlier
  and guesses; requiring the scheme is blunter and cannot guess wrong. Starting
  blunt.
- **Does anything want a *second* relay URL?** iroh's `RelayMap` holds many, and
  a deployment with two sites might want one each. One until somebody asks,
  because a list is a parsing decision and a precedence decision and neither is
  needed yet.
