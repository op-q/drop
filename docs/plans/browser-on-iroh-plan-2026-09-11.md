# Browser client on iroh plan

Status: **proposed — not scheduled**
Created: **2026-09-11**
Last updated: **2026-09-11**

## Goal

Rebuild the browser client as an **iroh node compiled to WebAssembly**, speaking
the same Drop conversation the CLI speaks, so that a browser transfer and a CLI
transfer differ in their carrier and in nothing else.

This is recorded now so the idea is not rediscovered as new. It is **not
scheduled**, it is blocked on work that has not happened, and
[Blockers](#blockers-in-the-order-they-have-to-be-solved) is the honest list of
why.

## Why this is worth doing: it removes a translator

The argument is not "browsers are nice to have." It is that the current design
has two dialects and a middlebox reconciling them, and this collapses that to
one conversation.

The relay does not merely forward. It **renames and invents control frames** —
recorded as a finding of item 3 phase 1 in
[`../implementation-checklist.md`](../implementation-checklist.md), and stated
in the transport trait's own documentation at
[`transport/mod.rs:100-106`](../../cli/src/transport/mod.rs#L100-L106):

> `receiver_connected` is a sentence the relay invents; no peer ever sends it.
> A path that blocked on it would be a path that only works over a relay, which
> is exactly what this phase is undoing.

[`../decisions.md`](../decisions.md) entry 12 settled that the control
vocabulary is the peer's and a relay only embellishes it. The browser client is
the last consumer of the embellished version. Removing it removes the reason the
embellishment has to keep working.

Under this plan:

```text
Browser (iroh compiled to wasm)
  Drop control frames + sealed chunks          ← identical to the CLI
    iroh QUIC connection
      WebSocket ─────────────► iroh relay ──UDP──► CLI peer
                               knows nothing about Drop
```

Against today:

```text
Browser
  Drop control frames + sealed chunks, relay dialect
    WebSocket ─────────────► Drop relay (api) ─────► CLI peer
                             knows codes and sessions,
                             invents control frames
```

The WebSocket does not disappear — it moves **beneath** the Drop protocol
instead of being it. It becomes the browser's substitute for the UDP socket the
sandbox will not give it, which is what iroh already uses it for.

## What a browser can and cannot do

Verified against iroh's documentation on 2026-09-11. This section is the reason
the plan is shaped the way it is, so it is worth re-checking before building:
iroh's browser support is moving.

**Can:** compile to `wasm32-unknown-unknown` via `wasm-bindgen` and join the
iroh network as an ordinary endpoint, with connections end-to-end encrypted such
that the relay cannot read them.

**Cannot, and this is not a temporary gap:** open a UDP socket, and therefore
hole-punch. iroh's own wording is *"we can't port our hole-punching logic in
iroh to browsers"* and *"all connections from browsers to somewhere else need to
flow via a relay server."* WebTransport with `serverCertificateHashes` and
WebRTC are both named as possible future routes to a direct browser connection
and neither is implemented.

**So a browser peer is permanently relayed.** Drop's headline claim is unharmed
— "no Drop server" stays precisely true, because an iroh relay is not a Drop
server — but a browser transfer can never be the direct path, and no amount of
later work in this repository changes that. It must never be described as
peer-to-peer, which is what [AGENTS.md](../../AGENTS.md) already requires.

## Blockers, in the order they have to be solved

### 1. The transport trait's futures are `Send`

The largest structural blocker, and it sits inside the abstraction that was
built to make this kind of thing possible.

Every method on `Transport` declares `+ Send` on its returned future —
[`transport/mod.rs:108`](../../cli/src/transport/mod.rs#L108),
[`116`](../../cli/src/transport/mod.rs#L116),
[`122`](../../cli/src/transport/mod.rs#L122),
[`131`](../../cli/src/transport/mod.rs#L131),
[`136`](../../cli/src/transport/mod.rs#L136) — and the trait's doc comment at
[line 73](../../cli/src/transport/mod.rs#L73) explains that it is deliberate:
the transfer paths are spawned onto a multi-threaded runtime by the CLI's own
tests, and leaving `Send` to inference would produce errors at the call site
rather than here.

On `wasm32-unknown-unknown` there are no threads and JS-backed futures are
`!Send`. A wasm transport cannot satisfy the bound.

This is a known shape rather than a novel problem — n0 hit it in iroh itself and
solved it with `n0-future`'s conditional `Send` aliases, which resolve to `Send`
on native targets and to nothing on wasm. Adopting the same approach is the
likely fix, and it touches every implementation of the trait plus the two
transfer paths. **It is a refactor of shipped, tested code for the benefit of
code that does not exist yet**, which is the main reason this plan is not
scheduled.

### 2. The browser cannot reach the DHT

Rendezvous today is `pkarr` with `features = ["dht"]`, talking to mainline
directly. iroh documents `discovery-pkarr-dht` as **incompatible with wasm**,
and the underlying reason is the same one as blocker 1's: no UDP.

A browser needs pkarr over HTTP relay instead. That is a second piece of
infrastructure, and it does not fit the existing configuration surface:
`DROP_RENDEZVOUS_BOOTSTRAP` is documented as comma-separated `host:port` DHT
nodes, which is not what a pkarr HTTP relay is.

Worse, it has to inherit entry 15's load-bearing rule — **a malformed value is
an error rather than a silent return to the public default** — in a context
where the failure is even quieter, because a browser user cannot read a terminal
error. An operator who meant to keep rendezvous inside their network and
silently got the public relay has lost exactly what they configured.

### 3. `peers_enforce_one_guess` needs a third answer

[`transport/mod.rs:94`](../../cli/src/transport/mod.rs#L94) has no default *on
purpose*: `false` on a direct connection is an unlimited guessing oracle, and
`true` over the relay fails every transfer. A new carrier must answer or fail to
compile.

A browser-over-iroh transport is a genuinely third case. There is no Drop relay
refusing a second claim — an iroh relay knows nothing about Drop sessions — so
the relay's `false` is wrong. But `true` means the browser runs entry 13's
one-guess checkpoint, including `AskTheTerminal`, which has no terminal to ask.

**This is a security decision requiring a decisions.md entry, not a plumbing
choice**, and it is why blocker 4 is ordered where it is.

### 4. `meta_ok` must land first

The checkpoint the answer to blocker 3 depends on is the one
[`meta-ok-key-confirmation-plan-2026-08-31.md`](meta-ok-key-confirmation-plan-2026-08-31.md)
exists to fix. That plan's own note — *"Land it before the direct path ships —
after that it is a wire break"* — is already overdue; the direct path shipped in
`v0.2.0`.

Building a browser transport against the unfixed frame means taking the same
wire break twice, once for the CLI and once for the browser. Doing `meta_ok`
first costs nothing here and saves a second breaking release.

### 5. The transfer paths touch a filesystem

`send::run` and `recv::run` are not reusable as they stand:

- [`payload.rs:104`](../../cli/src/payload.rs#L104) spools a compressed payload
  to `std::env::temp_dir()`, with the whole `SpoolFile` lifecycle and its
  signal-handler deletion path built around a real file.
- [`recv.rs:516`](../../cli/src/recv.rs#L516) creates the destination with
  `fs::File::create`, and `untar.rs` writes a tree.

A browser has neither. It has the File System Access API where available, and
memory where not — a distinction the current client already contends with, per
the direct-to-disk item in [`release-checklist.md`](../release-checklist.md).

So the reuse is real but partial: the **middle** of the stack — framing, the
envelope, the control conversation, the transport trait — is shared, and **both
ends** are per-platform. That is a defensible architecture, but it should be
understood as one before starting, not discovered in phase 3.

### 6. Toolchain and manifest

`iroh` requires `default-features = false` on wasm, which drops metrics. It also
sets `rust-version = "1.91"`, whereas
[`crypto/Cargo.toml:5`](../../crypto/Cargo.toml#L5) deliberately stays at
`1.85` *because* it compiles to wasm — a property that a new wasm crate pulling
in iroh does not inherit.

## Dependencies

This plan is blocked on, in order:

1. [`browser-client-removal-plan-2026-09-11.md`](browser-client-removal-plan-2026-09-11.md)
   — start from a clean slate rather than converting `App.svelte` in place.
   Almost nothing in the current client survives contact with this design, and
   keeping it alive during the rebuild means maintaining two browser clients.
2. [`meta-ok-key-confirmation-plan-2026-08-31.md`](meta-ok-key-confirmation-plan-2026-08-31.md)
   — blocker 4.
3. The conditional-`Send` refactor of the transport trait — blocker 1. Worth
   scoping as its own change against the existing CLI, where it is reviewable
   with the current tests, rather than as phase 1 of a wasm project.

## What does not change

- **The claim invariants.** A browser still runs code the site delivered, so
  [AGENTS.md](../../AGENTS.md)'s rule that browser transfers are only as strong
  as the delivered code holds exactly as written, under any transport. This plan
  is a reason to **keep** those rules through the removal, not to retire them —
  see that plan's open question 3.
- **"No Drop server."** An iroh relay is not a Drop server, so the claim stays
  precise. But a browser needs a site to be served from and a relay it can
  reach, so the browser path always involves somebody's infrastructure and must
  say so.
- **The envelope.** `drop-crypto` compiled to wasm is how the browser gets the
  envelope today and would still be how it gets it. Entry 11's reasoning — one
  implementation, not two — is the part of the current design that is right and
  survives.
- **The CLI.** Nothing here changes `drop send` or `drop recv`. To a CLI peer, a
  browser is a peer that never punches through, which iroh already handles.

## Open questions

### Open question 1 — is a browser client wanted at all?

The honest prior question. A browser client that can never be direct, needs two
pieces of relay infrastructure, and duplicates a CLI that installs in one
command may not earn its maintenance. The case for it is reach: no install, and
a receiver who has never heard of Drop.

This should be answered before any of the blockers are worked, because blocker 1
is a refactor of shipped code and is only worth doing if the answer is yes.

### Open question 2 — who runs the relays?

A browser needs an iroh relay and a pkarr HTTP relay. n0 runs public ones; a
deployment that cares runs its own via the entry 15 variables, once blocker 2
extends them. Neither is a Drop server, but "no install, no configuration"
quietly means "n0's infrastructure" for the default browser user, and that
belongs in the README rather than in a plan.

### Open question 3 — does `api` survive this?

If the browser stops using the Drop relay, `api`'s remaining job is
`--transport relay` for UDP-blocked networks. That is real and `netlab` covers
it. But it is worth asking whether an iroh relay — which already carries
connections that cannot hole-punch, and which entry 15 already lets a deployment
self-host — subsumes it. If it does, Drop ends with one relay concept instead of
two, which is the same simplification this plan makes for the browser.

Explicitly **not** proposed here. Noted because it is the natural next question
and should be asked deliberately rather than drifted into.

## Sources

- [Iroh — WebAssembly and Browsers](https://docs.iroh.computer/deployment/wasm-browser-support)
- [Iroh & the Web](https://www.iroh.computer/blog/iroh-and-the-web)
- [Tracking: WebAssembly support for iroh, n0-computer/iroh#2799](https://github.com/n0-computer/iroh/issues/2799)
- [Implementing a WebRTC Transport, n0-computer/iroh#4024](https://github.com/n0-computer/iroh/discussions/4024)
