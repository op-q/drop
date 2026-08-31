# Network lab plan

Status: **proposed**
Created: **2026-08-31**
Last updated: **2026-08-31**

## Goal

A `netlab/` directory that builds the real `drop` and `api` binaries and runs
them against constructed network topologies — NAT, latency, loss, blocked
UDP — inside Linux network namespaces, so the claims in
[`peer-to-peer-transport-plan-2026-08-20.md`](peer-to-peer-transport-plan-2026-08-20.md)
that end "cannot be verified on the development machine" acquire evidence.

Each run asserts four things: the file arrived byte-identical, which carrier
carried it, whether fallback fired when it should have, and what throughput was
achieved.

## Branch state, corrected

The kickoff brief for this work says Phase 4 lives on
`origin/feat/transport-selection`, two commits ahead of `main`, and to branch
from there. **That is no longer true.** It merged as pull request #48
(`98f1598`) and the remote branch is gone; `main` has since moved on through
`a2d73cb` and the 0.2.0 release merge. `cli/src/direct.rs`, `--transport`,
`DROP_TRANSPORT` and the locally drawn nameplate are all on `main` at
`219c417`, which is where this branch starts. Nothing is lost by the change.

One further brief instruction is stale: it asks for `netlab/` to be added to
"the directory tree in the root `README.md`". `a2d73cb` split the README into a
doc set and removed that tree. The surviving equivalents are the `Layout` block
and the `Document ownership` table in the documentation map,
[`docs/README.md`](../README.md), so the lab is registered there instead, and
the root README gains a one-line pointer rather than a tree it no longer has.

## Why Python and not Rust

The repository is a Cargo workspace and its tests are Rust, so choosing another
language needs a reason better than preference.

**The transport logic stays in Rust because that is where the abstractions
are.** `Transport`, `Frame`, the envelope, the framing, the one-guess policy —
all of it is already modelled, already tested, and already has a second
implementation of its trait to keep the seam honest. Nothing in this work
belongs inside that boundary, and anything that ended up there would be a
duplicate of something `cli/tests/transfer.rs` already covers.

**What this lab actually does is shell orchestration and data analysis**, and
neither is transport logic:

| The work | What it is |
| --- | --- |
| `ip netns add`, `ip link set`, `iptables -t nat`, `tc qdisc` | driving external commands and reading their exit codes |
| Spawning two binaries, feeding one the other's code | process supervision |
| Matching a status line, comparing checksums | text and byte handling |
| Throughput against `window / RTT`, across a matrix | arithmetic over a table of runs |
| A dated Markdown report | templating |

Rust can do all of it. It would need `std::process` wrappers around the same
`ip` invocations, a test harness that is not `cargo test` because these runs
take minutes and must not join the workspace's test set, and a reporting layer.
The result would be a second Rust crate in the workspace that shares no types
with the first and exists to shell out.

Three specific reasons Python wins here:

1. **It stays outside the workspace.** `cargo test --workspace --all-targets`
   is the command AGENTS.md tells every agent to run. A lab that needs
   `CAP_NET_ADMIN` and takes minutes must not be reachable by it, and a crate
   in the workspace is reachable by it by default.
2. **`pytest` gives the matrix for free.** Six topologies against a shared
   fixture set is parametrisation, and skipping cleanly when the kernel will
   not grant a namespace is `pytest.skip`. Building that in Rust is building a
   test framework.
3. **The lab is not shipped.** It is not in the binary, not in the web build,
   and not on any user's machine, so the dependency-weight argument that
   governs `cli/Cargo.toml` does not apply to it.

The line to hold, and it is the same line
[`../decisions.md`](../decisions.md) entry 11 draws for the browser: **no part
of the Drop protocol is reimplemented in Python.** No envelope, no handshake,
no framing, no chunk sealing. The lab starts real binaries and inspects what
comes out of them. A Python implementation of any of it would reintroduce
exactly the two-implementations drift entry 11 exists to prevent.

## Two findings that change the shape of this work

Both were established by probing the machine before writing this plan, and both
contradict an assumption in the brief. They are the reason the phase order
below is not the brief's suggested order.

### Finding 1 — this needs no root, and the skip condition is not `CAP_NET_ADMIN`

The brief assumes the lab runs privileged and skips when `CAP_NET_ADMIN` is
absent. On this machine the process has **no capabilities at all**
(`CapEff: 0000000000000000`) and the full lab still works, because unprivileged
user namespaces are enabled:

```text
$ unshare -Urnm bash -c '...'
CapEff: 000001ffffffffff          # full capabilities *inside* the namespace
--- ping ---     2 packets transmitted, 2 received, 0% packet loss
--- netem ---    rtt min/avg/max/mdev = 50.028/50.029/50.030/0.001 ms
--- nat ---      nat ok
```

Verified inside one `unshare -Urnm`: two child network namespaces, a veth pair
moved between them, addressing, `tc qdisc ... netem delay 50ms` measured at the
requested 50 ms, and an `iptables -t nat` MASQUERADE rule accepted.

So the entry condition is **"can this process obtain `CAP_NET_ADMIN` in a
network namespace"**, which has three answers worth distinguishing, and the
fixture must try them in order rather than testing for one:

1. Unprivileged user namespace — preferred, needs no privilege at all. The lab
   re-executes itself into `unshare -Urnm`.
2. Real `CAP_NET_ADMIN` (root, or a granted capability) — used directly.
3. Neither — `kernel.unprivileged_userns_clone=0`, a seccomp filter, or a
   restricted container. **Skip**, with a message naming which of the three was
   tried and what it returned.

The third case is real and is what GitHub-hosted runners must be checked
against rather than assumed; that check is a phase-0 task, not an assumption.

### Finding 2 — the direct path cannot run in a hermetic lab as the code stands

This is the substantive finding, and it invalidates rows 1, 2 and 3 of the
brief's topology matrix as written. **Three independent parts of the direct
path reach the public internet, and an isolated namespace has none of it.**

| Where | What it needs | In an isolated netns |
| --- | --- | --- |
| `MainlineDirectory::new` → `pkarr::Client` | the mainline DHT's public bootstrap routers | never bootstraps |
| `QuicEndpoint::bind` → `RelayMode::Default`, then `online()` | one of n0's public relays, within `ONLINE_TIMEOUT` (15 s) | fails after 15 s |
| `rendezvous::publishable` | at least one globally routable address, or a relay URL | **has neither** |

The third is the one that cannot be worked around by adding a route, and it
deserves spelling out because it is a *deliberate* property rather than an
oversight. [`../decisions.md`](../decisions.md) entry 14 strips every private
address from a published record so a stranger who guesses a nameplate cannot
map the sender's internal network. `is_routable_v4` rejects RFC 1918, loopback,
link-local, multicast, carrier-grade NAT (100.64/10), the benchmarking range
(198.18/15), and **the documentation ranges 192.0.2/24, 198.51.100/24 and
203.0.113/24** — which is to say, every address block a lab is entitled to use.
Entry 14's own worked example publishes `203.0.113.44`, and the shipped filter
would refuse it.

Entry 14 states this consequence in its own words: "Two peers on the same LAN
can no longer find each other directly through the DHT, because the address
that would let them is exactly the one being withheld." A netns lab is a LAN.
The decision is right and this plan does not propose weakening it.

**What follows from this**, and it is the plan's central choice:

- Every topology that exercises **the relay path** — UDP blocked, injected RTT,
  packet loss, and the fallback report — runs today against the shipped
  binaries with no source change of any kind. That is four of the six rows,
  including the one that carries the README's throughput claim.
- Every topology that exercises **the direct path** — plain LAN with no relay,
  full-cone NAT, symmetric NAT — needs hermetic rendezvous, and there is no way
  to get it without either giving the lab real internet access or letting the
  CLI be pointed at rendezvous infrastructure other than the public default.

The second half is an open question for the owner, not a decision to make
inside a test directory. It is written up under
[Open question 1](#open-question-1--how-the-direct-path-becomes-testable) with
three candidate answers and a recommendation, and Phase 4 does not start until
it is answered.

**The consequence for sequencing:** the relay topologies come first. They are
worth having on their own, they exercise the whole namespace and measurement
apparatus, and they prove the plumbing before the plumbing is asked to carry
the harder question.

## Constraints and invariants

- **No Drop protocol in Python.** Stated above; it is the one rule that must
  not bend.
- **No framework.** No scenario DSL, no plugin registry, no abstract base class
  for a harness with one implementation. Eight files, each of which does one
  thing, and a topology is a function that returns a description of a network.
- **No existing Rust test is migrated or duplicated.** `cli/tests/transfer.rs`,
  `cli/tests/archive.rs`, `cli/tests/protocol.rs` and `tests/` stay exactly
  where they are and keep their coverage.
- **No runtime configuration is added to the relay for a test's convenience.**
  The bounds in [`../../src/config.rs`](../../src/config.rs) are compile-time on
  purpose. A topology that needs a different limit does not get one.
- **This never blocks pull-request CI.** Separate workflow, nightly plus
  `workflow_dispatch` plus a label, and a clean skip when the kernel refuses a
  namespace.
- **Synthetic everything.** Generated payloads, drawn codes, and the addressing
  documented below. No real code, filename, IP address or personal path reaches
  a fixture, an assertion, a report or this plan.
- **The lab reports, it does not launder.** A run that could not establish a
  condition says so and fails; it never downgrades to a weaker topology and
  passes.

## Non-goals

- Replacing the Rust test suite, or moving any part of it.
- Testing the web client. A browser needs the relay and has no namespace story.
- Carrier-grade NAT, real cellular networks, or IPv6 topologies. The first two
  are out of reach and the third is worth doing only after IPv4 works.
- Performance regression tracking over time. The throughput lane checks a
  documented claim; it is not a benchmark suite and produces no baseline to
  defend.
- Running against the public relay, ever. `docs/validation/` already forbids it
  for shakeouts and the same rule applies here.

## The topology matrix

Addressing is fixed and documented so a report never has to quote an address
that could be mistaken for a real one. All segments come from `10.0.0.0/8`,
chosen because it is unambiguously private, is what the `publishable` filter
rejects first, and can never be confused with a routable address in a report.

```text
        ┌──────────┐        ┌──────────┐        ┌──────────┐
        │  sender  │────────│  router  │────────│ receiver │
        │ 10.10.0.2│  left  │ .1   .1  │ right  │ 10.20.0.2│
        └──────────┘        └────┬─────┘        └──────────┘
                                 │ public segment, 10.30.0.0/24
                            ┌────┴─────┐
                            │  relay   │   only in topologies that need one
                            │ 10.30.0.2│   (`api`, bound to 10.30.0.2:8080)
                            └──────────┘
```

Four namespaces, not the three the brief names. The fourth exists because
putting the `api` process in the router namespace would place a Drop server
*inside* the NAT boundary being tested, which is the opposite of the
deployment being modelled and would make the NAT topologies meaningless. The
relay namespace is created only for topologies that use it, so the "no relay
process running" row is literally true rather than a relay that was merely
unused.

| # | Topology | Path exercised | Proves | Blocked on |
| --- | --- | --- | --- | --- |
| 1 | Plain LAN, no relay namespace at all | direct | a transfer completes with no Drop process anywhere | open question 1 |
| 2 | Full-cone NAT both ends | direct | hole punching succeeds | open question 1 |
| 3 | Symmetric NAT both ends | direct → relay | hole punching fails and the fallback is clean | open question 1 |
| 4 | UDP dropped at the router | relay | falls back, completes, and *says* it fell back | nothing |
| 5 | RTT injected, several values | relay | throughput tracks `window / RTT` | nothing |
| 6 | 1% loss, both directions | relay | no corruption, no indefinite hang | nothing |

Rows 4, 5 and 6 correspond to lanes 5, 4 and 6 of
[`../validation/transfer-shakeout-template.md`](../validation/transfer-shakeout-template.md)
respectively, and are the parts of that template a human cannot perform by hand.
Rows 1 to 3 are the unchecked boxes under **Validation** in the peer-to-peer
plan.

## Phases

### Phase 0 — Machine-readable path reporting, in Rust — **done 2026-08-31**

**A separate, self-contained commit with its own Rust test, landing before any
Python exists.** Today the carrier is human prose on stderr:

```text
Path    peer-to-peer (no Drop server)
Path    relay (encrypted; the relay cannot read it)
No peer-to-peer path: {error}
Falling back to the relay.
```

`cli/src/direct.rs:report` writes the first two and `send.rs` / `recv.rs` write
the rest. Asserting on any of it couples the lab to wording that exists to be
read by a person, and the first sympathetic rewrite breaks every test.

- [x] A stable status line alongside the prose, not replacing it. Emitted by
      both halves, on stderr, one line, machine-first:

      ```text
      drop-status: path=p2p fallback=none
      drop-status: path=relay fallback=rendezvous
      drop-status: path=relay fallback=none
      ```

      `path` is `p2p` or `relay` and is the carrier that actually moved bytes.
      `fallback` is `none`, `rendezvous` (setup failed), or `no-record` (the
      receiver looked and found nothing, meaning the sender fell back).
- [x] Behind `--status` / `DROP_STATUS`, off by default, because a line of
      `key=value` in a user's terminal is noise and the prose is the product.
      The variable can only turn it on, so a harness exports it once.
- [x] Rust test in `cli/tests/transfer.rs` asserting the line for a relay
      transfer, and a unit test in `direct.rs` for the rendering of each
      variant.

Deliberately **not** a `--json` mode. A JSON object invites the growth of a
reporting schema nobody asked for, and the lab needs one fact: which carrier,
and why. One line is smaller, is greppable from a shell, and does not become a
compatibility surface.

Files: `cli/src/direct.rs`, `cli/src/send.rs`, `cli/src/recv.rs`,
`cli/src/main.rs` (flag and help), `cli/tests/transfer.rs`.

#### Findings

Done 2026-08-31. 156 tests, up from 153.

**The prose and the tag cannot be held apart by a test, and trying was a
mistake worth recording.** The first draft asserted that no carrier's tag is a
substring of its own prose, meaning to pin that a reworded sentence could never
reach the machine line. It failed immediately and correctly: the relay's prose
is `relay (encrypted; the relay cannot read it)` and its tag is `relay`, which
is simply the right word in both places. The property was an accident of the
direct carrier's wording rather than a design rule, and the test was deleted
rather than weakened. What actually protects the line is the exact-string
assertion over all four combinations — a rewording that reached the tag fails
it by definition.

**The end-to-end assertion had to spawn the binary.** Everything else in
`cli/tests/transfer.rs` calls `send::run` and `recv::run` in-process, which
would have pinned the string while leaving the flag parsing, the option
plumbing and the choice of stream unchecked — and those are exactly what a lab
spawning `drop` depends on. The test runs `CARGO_BIN_EXE_drop` as a
subprocess, reads the code off its stdout, and matches whole lines of stderr.
Asking with `--status` on one half and `DROP_STATUS` on the other covers both
entry points in one transfer.

**A second test asserts the line is absent by default**, because the first one
alone would pass just as well if the line were unconditional, and an unasked-for
`key=value` in a user's terminal is the thing the flag exists to prevent.

### Phase 1 — Namespaces, one topology, one passing test — **done 2026-08-31**

The narrowest thing that proves the plumbing. **Topology 4** rather than the
brief's suggested topology 1, because 4 is the simplest one that is not blocked
on open question 1.

- [x] `netlab/netns.py` — namespace lifecycle, veth pairs, addressing, routes,
      NAT rules, `tc netem`, and a context manager that tears down on every
      exit path including a failed assertion. Everything is `ip`, `iptables`
      and `tc` driven through `subprocess`; nothing here parses a packet.
- [x] `netlab/conftest.py` — build the binaries once per session with
      `cargo build --workspace --bins`; establish namespace capability by the
      three-way probe in finding 1; skip with a specific message if none works.
- [x] `netlab/runner.py` — spawn `drop send` and `drop recv` in their
      namespaces, carry the code from one to the other, capture both streams,
      enforce a wall-clock timeout, and kill the process group on the way out.
- [x] `netlab/topologies.py` — `udp_blocked()` only, so far.
- [x] `netlab/test_transfer.py` — one test: a 4 MiB synthetic payload crosses,
      the SHA-256 matches, and `drop-status:` reports `path=relay`.
- [x] `netlab/pyproject.toml`, `netlab/README.md` including the
      "what this does not prove" section.
- [x] Housekeeping: `__pycache__/` and `.venv/` in `.gitignore`, a `pip`
      ecosystem entry in `.github/dependabot.yml`, and `netlab/` registered in
      the documentation map [`docs/README.md`](../README.md).

Gate as written: *this test fails if the `iptables` rule that drops UDP is
removed.* **That gate is wrong and was not met — it cannot be.** See the
findings below; it is replaced by two negative controls that do work.

#### Findings

Done 2026-08-31. Three topologies, three tests, 51 seconds.

**The stated gate was unmeetable, and noticing why is the most useful thing
this phase produced.** In a lab with no route to the internet, the direct path
cannot be set up whether or not the router forwards UDP — `online()` times out
either way. So removing the `iptables` rule leaves `udp_blocked` passing, and
the topology cannot attribute the fallback to the block. This is precisely the
risk the plan's own list names as "the lab passes while proving less than it
looks like", met on the first topology attempted.

It is not fixable by trying harder, and it is the *same* blocker as open
question 1: attributing a fallback to a cause requires the direct path to be
able to succeed when the cause is absent. What the topology does honestly show
— the fallback fires, the transfer completes across a routed network, and the
carrier is reported — is worth having, and is now what the test claims. Both
the test and `netlab/README.md` say what it does not show.

The gate is replaced by two negative controls that do discriminate, in
`test_the_topology_is_load_bearing`: with the router not forwarding, the ends
cannot reach each other at all; with no relay process running, a relayed
transfer fails. The first proves the namespaces are really separated by the
router, the second that the relay is really carrying the bytes.

**Nothing here needs root, and it took one line to find out.** Finding 1 held:
the whole suite runs as an ordinary user. The re-execution into `unshare -Urnm`
had one non-obvious requirement — pytest has already replaced file descriptors
1 and 2 with its own capture buffers by the time any hook runs, and a process
that `exec`s inherits them, so the re-executed session wrote its entire output
into a buffer no surviving process would ever read. The run appeared to pass
instantly and silently. Suspending the capture manager before the `exec` fixes
it; doing the `exec` at conftest import time cannot, which is why it happens in
`pytest_configure`.

**A namespace directory needs a tmpfs, because fake root is not root.** Inside a
user namespace this process is uid 0, but file ownership is not remapped, so
`/run` still belongs to real root and `mkdir /run/netns` fails. A tmpfs mounted
over the namespace directory is writable and, being in a private mount
namespace, invisible to the host.

**A failed transfer is a result, not a lab error.** The first draft raised from
the runner when the sender died before announcing a code, which made "the relay
is not running" indistinguishable from "the lab is broken" — and the negative
control that depends on that difference had to catch a bare `Exception`. The
runner now returns a failed `Transfer`, and `LabError` means only what its
docstring says: the network could not be built as asked.

**Timings, for calibration.** A 4 MiB relayed transfer across the router: ~11 s
wall including relay startup. The same on `auto` with no internet: ~26 s, of
which 15 s is `ONLINE_TIMEOUT` before the sender gives up on a home relay. That
15 s is a floor under every `auto` topology in this lab and should be assumed
in Phase 2's budget.

### Phase 2 — Latency and the `window / RTT` claim

[`../protocol.md`](../protocol.md) line 356 states 16 MiB in flight and 4 MiB
acknowledgement batches; `cli/src/send.rs:19` is where `WINDOW_BYTES` lives.
The derived claim is that on a high-latency link throughput approaches
`WINDOW_BYTES / RTT` regardless of available bandwidth.

**Checking that needs more care than one measurement.** At the brief's 100 ms
the ceiling is 160 MiB/s, which is far above what a veth pair carrying AES-GCM
through a relay process will reach — so a single run at 100 ms would pass while
measuring the pipeline's own speed and proving nothing about the window.

- [ ] Measure at three RTTs chosen so the window is the binding constraint:
      200 ms (80 MiB/s ceiling), 400 ms (40), 800 ms (20). Confirm during
      implementation that the unconstrained rate on this link comfortably
      exceeds 80 MiB/s; if it does not, raise every value until it does and
      record the measured unconstrained rate in the report.
- [ ] Assert two things, and the second is the real one:
      - throughput never **exceeds** `WINDOW_BYTES / RTT`, which would mean the
        window is not being enforced;
      - throughput is **inversely proportional to RTT** across the three
        points, within a tolerance wide enough for scheduler noise — a doubled
        RTT halves the rate. That relationship is what the claim asserts, and
        it cannot be satisfied by accident.
- [ ] Netem `delay` is applied on both router interfaces at half the target, so
      the end-to-end round trip is the stated figure rather than double it.
      Assert the achieved RTT with `ping` before the transfer, and fail if the
      network is not the network that was asked for.
- [ ] Record the numbers rather than only the pass. A ratio that is right for
      the wrong reason is visible in the table and invisible in a green tick.

Files: `netlab/topologies.py`, `netlab/test_transfer.py`, `netlab/netns.py`.

### Phase 3 — Loss

- [ ] 1% loss in both directions, applied at the router. A 16 MiB payload, so
      loss is certain rather than probable.
- [ ] Assert byte-identical arrival and completion inside a bounded wall clock.
      The second half is the point: the failure this looks for is a transfer
      that never finishes and never errors, which is what a flow-control or
      acknowledgement bug looks like from outside.
- [ ] A deliberately harsher run — 5% — recorded but **not asserted on**, so
      the report carries a data point about degradation without turning a
      probabilistic outcome into a flaky test.

### Phase 4 — The direct path

**Does not start until [open question 1](#open-question-1--how-the-direct-path-becomes-testable)
is answered.** Written here so the shape is visible, with checkboxes that stay
unticked and honest.

- [ ] Whatever hermetic rendezvous the answer requires.
- [ ] Topology 1 — plain LAN, no relay namespace, no `api` process anywhere.
      The strongest demonstration the peer-to-peer plan asks for, and the one
      its Validation section lists first.
- [ ] Topology 2 — full-cone NAT at both ends: `MASQUERADE` with the
      conntrack behaviour that lets an unrelated source reach an existing
      mapping. Assert `path=p2p`.
- [ ] Topology 3 — symmetric NAT at both ends: per-destination port mapping, so
      the mapping the peer learned is not the mapping it can use. Assert the
      fallback fires, that it is `fallback=rendezvous` or `no-record` rather
      than a crash, and that the file still arrives.
- [ ] Assert that topology 1 runs with **no listening Drop process**, checked
      directly rather than assumed — the run fails if an `api` process exists.

### Phase 5 — Reporting and CI

- [ ] `netlab/report.py` — writes `docs/validation/network-lab-<NNN>-YYYY-MM-DD.md`
      from the run results, following the numbering and the report
      requirements of the shakeout template: revision tested, environment,
      what was skipped and why, a table of topology against outcome and
      measured throughput, and findings.
- [ ] `.github/workflows/netlab.yml` — nightly `schedule`, plus
      `workflow_dispatch`, plus a `netlab` pull-request label. Never on
      `pull_request` unlabelled and never on `push`.
- [ ] Confirm on a GitHub-hosted runner which of finding 1's three cases
      applies. If it is the third, the workflow's value is
      `workflow_dispatch` on a self-hosted or privileged runner and the
      nightly is deleted rather than left failing.
- [ ] Mirror status into [`../implementation-checklist.md`](../implementation-checklist.md)
      as a new item, in the same change as the behaviour.

## Risks

- **The lab passes while proving less than it looks like.** This is the exact
  failure the peer-to-peer plan documents about its own loopback tests, and a
  netns lab is a richer environment making the same mistake more convincingly.
  The mitigation is the negative control: every topology must be shown to fail
  when its defining condition is removed, and that shownness belongs in the
  report.
- **A netns NAT is not a NAT.** `iptables` MASQUERADE with `nf_conntrack` is a
  reasonable model of a home router and is not carrier-grade NAT, is not a
  hardware appliance, and does not have the mapping timeouts or the port
  exhaustion of either. Topology 2 passing means "hole punching works against
  this model", never "hole punching works".
- **Symmetric NAT is hard to construct convincingly.** The distinction that
  matters is whether the external port depends on the destination, and
  `--random-fully` alone does not give that. Getting it wrong makes topology 3
  a full-cone test that passes for the wrong reason — and it will *look*
  right, because the fallback still fires whenever the direct path fails for
  any reason. Verify the mapping behaviour directly before trusting the row.
- **Flakiness becomes noise and then becomes ignored.** Six topologies, real
  timing, and a nightly schedule is a recipe for a permanently red badge that
  nobody reads. Loss is asserted only at a level where success is near-certain,
  throughput is asserted as a relationship rather than a number, and anything
  that cannot be made reliable is recorded rather than asserted.
- **Timing assertions on shared CI hardware.** A GitHub runner is a noisy
  neighbour and the throughput lane is the one that will suffer. The
  inverse-proportionality assertion tolerates absolute slowness in a way an
  absolute threshold does not, which is part of why it was chosen.
- **Root-equivalence inside a user namespace is still a sandbox escape
  surface.** The lab runs the project's own binaries and nothing downloaded,
  but it does grant them `CAP_NET_ADMIN` in a namespace, and the `unshare`
  invocation must not be reachable from a test that takes input from anywhere
  but the repository.
- **Scope creep into a framework.** Six topologies is exactly the number at
  which an abstraction starts to look justified. It is not: a topology is a
  function that returns a description of a network, and the moment one grows a
  base class this plan has failed.

## What this lab will not prove

Written here so it can be copied into `netlab/README.md` rather than invented
twice, in the same spirit as the peer-to-peer plan's own section.

- **Not carrier-grade NAT.** See the risk above. Model, not appliance.
- **Not the mainline DHT.** Whatever answers open question 1, it is not the
  public DHT with millions of nodes, its churn, its latency, or its hostile
  participants. Rendezvous timing measured here says nothing about rendezvous
  timing in the field.
- **Not n0's relay infrastructure**, its regions, or its capacity.
- **Not real-world path characteristics.** `netem` produces uniform delay and
  independent uniform loss. Real networks produce bursts, reordering,
  bufferbloat and asymmetry, and a transfer that survives 1% independent loss
  has not been shown to survive 1% bursty loss.
- **Not IPv6, not dual-stack**, and therefore not the address-family selection
  a real deployment does.
- **Not the browser.** No topology here involves the web client.
- **Not throughput on any real link.** The numbers are veth numbers. What they
  test is the *shape* of the relationship between window and RTT, which is a
  property of Drop's flow control and not of the wire.
- **Not the security properties.** Nothing here checks encryption, the one-guess
  policy, or the address filter. Those are covered in Rust, and a network
  topology adds nothing to them.

## Validation

- [ ] Every topology fails when its defining condition is removed, demonstrated
      once per topology and recorded in the report.
- [ ] `cargo test --workspace --all-targets` unaffected by anything in this
      work, and the Phase 0 commit adds passing Rust tests to it.
- [ ] `cargo fmt --all -- --check` and
      `cargo clippy --workspace --all-targets --all-features -- -D warnings`
      clean after Phase 0.
- [ ] The lab skips cleanly, with a message naming what it tried, on a machine
      where no namespace can be obtained.
- [ ] `scripts/check-secrets.sh` clean.
- [ ] A full matrix run produces a report under `docs/validation/` containing
      no address outside the documented `10.0.0.0/8` plan and no real path.
- [ ] The peer-to-peer plan's Validation checkboxes are ticked **only** for
      topologies that actually ran, with the caveats from "what this lab will
      not prove" attached to each.

## Open questions

### Open question 1 — how the direct path becomes testable

Finding 2 is the blocker for half the matrix, and the answer is a decision
about production surface rather than about test code. Three candidates:

**A. Give the lab real internet.** The router namespace forwards to the host's
uplink, so the public DHT and n0's relays work as they do in the field.
*Against:* every run publishes a real record to a public DHT, which the
peer-to-peer plan already flags as an outward-facing action deserving a
deliberate decision rather than something that happens inside a test run. It
makes the suite depend on third-party availability, adds three to five seconds
of bootstrap per run, and — worst — the record would carry **the host's real
public IP address**, which is precisely what this repository forbids putting in
a report. Also self-defeating: a NAT topology whose peers can reach the real
internet is not testing the NAT.

**B. Run rendezvous infrastructure inside the lab.** A local `iroh` relay in
the public segment, and either a local DHT bootstrap or a local pkarr relay.
This needs the CLI to accept both as configuration — new production surface,
and the plan's rule against adding runtime configuration for a test's
convenience points straight at it.

The argument that it is *not* merely test convenience: a self-hoster who runs
their own Drop relay today cannot run their own rendezvous, so a Drop
deployment inside an air-gapped or egress-filtered network cannot use the
direct path at all. That is a real deployment gap that exists independently of
this lab, and closing it would make the lab possible as a side effect rather
than as a motive. It also has a pleasing property worth noting: because
`is_publishable` accepts `TransportAddr::Relay(_)` unconditionally, peers whose
only publishable address is a local relay URL still produce a valid record —
and iroh will still attempt to punch a direct path using the addresses the
peers observe through that relay. So a local relay exercises **real hole
punching against the lab's NATs**, which is the thing topology 2 exists for.

*Against:* it is a feature, it needs its own plan and probably its own decision
entry, and it delays this work behind it.

**C. Accept the gap and ship rows 4 to 6.** The lab covers the relay path,
including the fallback report, the throughput claim and loss behaviour, and the
direct-path rows stay unchecked in the peer-to-peer plan with this document
naming exactly why. Honest, immediately deliverable, and leaves the feature's
premise as unverified as it is today.

**Recommendation: C now, B as its own piece of work.** C is most of the value
and is unblocked; B is the right long-term answer but is a product change
wearing a test's clothes, and deciding it inside this plan would be deciding it
in the wrong place. A should be rejected outright on the public-IP grounds
alone.

### The remaining questions

- **Does an unprivileged user namespace work on a GitHub-hosted runner?**
  Phase 5 checks it rather than assuming it. If not, the nightly is worthless
  and should not be written.
- **How large should the throughput payload be?** It must be several times the
  16 MiB window for the steady state to dominate the ramp-up, which suggests
  128 MiB or more — against a run time that is already RTT-inflated at 800 ms.
  Measure the ramp and pick from data.
- **Does the lab build in release or debug?** Debug is a faster build and a much
  slower AES-GCM, which pushes the unconstrained rate down and risks it falling
  under the window ceiling, making Phase 2's assertion untestable. Probably
  release for the throughput lane and debug elsewhere, decided by measurement.
- **Should the report be committed, or produced and discarded?** The shakeout
  template commits dated reports. A nightly that commits a file every night is
  a different thing, and the answer is probably to commit only runs a human
  asked for.
- **What is the `netlab` Python floor?** 3.12 is what is on this machine and on
  current runners. Nothing here needs anything recent, so the floor should be
  set by what CI provides rather than by what is convenient.
