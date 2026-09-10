# Network lab

Runs the real `drop` and `api` binaries inside Linux network namespaces, across
networks built to misbehave in a specific way, and asserts what came out.

Drop's Rust tests cover almost everything in-process and nothing that needs a
topology. Loopback has no router, no NAT and no round-trip time, so hole
punching, fallback and the acknowledgement window are all unexercised there —
and the endpoint-drop bug fixed during the transport work was invisible to
every loopback test for exactly that reason. This is the lab for that category
of bug.

The plan is
[`docs/plans/network-lab-plan-2026-08-31.md`](../docs/plans/network-lab-plan-2026-08-31.md).
Status is mirrored in
[`docs/implementation-checklist.md`](../docs/implementation-checklist.md), item 5.

## Running it

The lab needs `pytest`, and nothing else Python-side. It is not a dependency of
anything the project ships, so it lives in a virtual environment rather than on
the machine:

```bash
python3 -m venv netlab/.venv
netlab/.venv/bin/pip install 'pytest>=8'
```

Then, from the repository root:

```bash
netlab/.venv/bin/python -m pytest netlab/
```

Activating the environment first — `source netlab/.venv/bin/activate`, or
`activate.fish` — makes the shorter `pytest netlab/` work. Either way it needs
no privilege: see [Privilege](#privilege) for why, and for what happens if it
cannot get any.

`netlab/.venv/` is ignored by git, so this is a per-clone step. Everything else
the lab needs — `ip`, `iptables`, `tc`, `unshare`, `ping`, `curl` — is expected
on the machine, and a missing one is reported by name before any test runs.

A full run takes about three and a half minutes, most of it the latency lane
waiting out round trips it asked for. It ends with a `measured` table, printed
whether or not anything failed:

```text
=================================== measured ===================================
unimpaired         at least 611 MiB/s
ack loop 200ms     measured 200.4ms, ceiling 79.8 MiB/s, 37.3 MiB/s streaming ...
ack loop 400ms     measured 400.5ms, ceiling 40.0 MiB/s, 19.9 MiB/s streaming ...
ack loop 800ms     measured 800.2ms, ceiling 20.0 MiB/s, 9.4 MiB/s streaming ...
1% loss, observed  3.5% of probes lost sender to relay (two hops)
```

A pass is not the whole result of a lane that measures something: a ratio can
come out right for the wrong reason, and that is visible in the numbers and
invisible in a green tick.

**The binaries are built `--release`.** That is a correctness requirement, not
impatience. Debug moves about 6 MiB/s through this lab against roughly
600 MiB/s optimised — AES-GCM with its bounds checks left in — and every
throughput ceiling the latency lane reasons about sits *above* 6 MiB/s. Under
debug the acknowledgement window could never be the binding constraint, and
the lane would measure the missing optimiser and report it as a property of
the protocol.

## What is here

```text
netlab/
  netns.py         namespaces, veth pairs, NAT rules, tc netem, measurement
  topologies.py    the named network shapes
  runner.py        spawns the real binaries and reads what came out
  conftest.py      builds the binaries; gets a namespace or skips
  test_transfer.py the tests
  rendezvous/      a cargo project: an iroh relay and a small DHT, for the
                   direct-path topologies to meet through
```

`rendezvous/` is deliberately **not** a member of the repository's cargo
workspace. `cargo test --workspace --all-targets` is the command every
contributor runs, and a lab dependency that takes minutes to build has no
business being reachable by it. It is built by its own fixture, only when a test
that needs it runs.

## The network

Two shapes. All addressing is inside `10.0.0.0/8`, which is both private
and — not coincidentally — a range `rendezvous::publishable` refuses to put in
a published record, so nothing here can be mistaken for an address Drop would
disclose.

```text
        ┌──────────┐        ┌──────────┐        ┌──────────┐
        │  sender  │────────│  router  │────────│ receiver │
        │ 10.10.0.2│        │ .1    .1 │        │ 10.20.0.2│
        └──────────┘        └────┬─────┘        └──────────┘
                                 │ 10.30.0.0/24
                            ┌────┴─────┐
                            │  relay   │  only when a topology asks for one
                            │ 10.30.0.2│
                            └──────────┘
```

The relay gets its own namespace rather than sharing the router's. A Drop
server inside the router would sit *inside* the NAT boundary under test, and it
would make "no relay in the path" impossible to state honestly.

The direct-path topologies need a different shape, because hole punching is a
property of what happens when *both* peers are translated:

```text
  ┌────────┐      ┌───────┐ 10.50     ┌──────┐ 10.40  ┌────────────┐
  │ sender │──────│ nat-a │───────────│ core │────────│ rendezvous │
  │10.10..2│ 10.10│       │           │      │        │  10.40.0.2 │
  └────────┘      └───────┘           └──┬───┘        └────────────┘
  ┌────────┐      ┌───────┐ 10.60         │
  │receiver│──────│ nat-b │───────────────┘
  │10.20..2│ 10.20│       │
  └────────┘      └───────┘
```

Two NAT routers, a core that translates nothing, and **the rendezvous host on
its own point-to-point link**. That last detail is load-bearing rather than
tidy: a punched path runs `nat-a → core → nat-b` and never crosses the
rendezvous link, so the byte counters on that link say whether the relay carried
the payload or only introduced the peers. On a shared public segment the
punched traffic would cross the same wire and the measurement would be
meaningless. `rendezvous` runs `netlab-rendezvous`, never `api`.

## Topologies

| Topology | State | What it shows |
| --- | --- | --- |
| `routed_lan` | **runs** | a transfer completes across a router and two segments |
| `udp_blocked` | **runs** | the fallback fires, completes, and reports itself |
| `high_latency` | **runs** | throughput is bounded by `window / RTT` and halves as the RTT doubles |
| `lossy` | **runs** | 1% a hop arrives byte-identical and terminates |
| `plain_lan` | **runs** | a transfer completes with no Drop process running anywhere |
| `full_cone_nat` | **runs** | a direct path is established through a port-preserving NAT |
| `symmetric_nat` | **runs** | the punch fails, the connection survives the relay, the file arrives |

The last three became possible when rendezvous became configurable —
[`decisions.md`](../docs/decisions.md) entry 15 — so the lab can run its own
iroh relay and DHT instead of reaching the public ones it has no route to.

### How a hole punch is detected, and why not from `--status`

`drop --status` cannot answer it. `path=p2p` means "no Drop server was
involved", and that is true whether iroh punched through the NAT or carried the
same QUIC connection over its own relay — `cli/src/direct.rs` states the rule
outright: *a failed hole punch is not a failed connection*. Asserting
`path=p2p` for the full-cone row would therefore have tested nothing about
traversal, and the symmetric row's originally planned assertion
(`fallback=rendezvous`) does not happen at all, because rendezvous succeeds and
the Drop relay is never consulted.

So the punch is measured on the wire, and the NAT's own behaviour is measured
before the transfer:

- **Mapping**, by `observed_source_ports`: one socket sends to two destinations
  and the far end reports the source ports it saw. Equal means
  endpoint-independent and punchable; different means symmetric. Inferring this
  from the `iptables` rule instead would be unfalsifiable — a symmetric
  topology that is secretly full-cone still passes a test asserting the punch
  failed, because any failure satisfies it.
- **The path**, by `interface_bytes` on the rendezvous link. Kilobytes mean
  signalling; megabytes mean the relay was the path.

The two NAT topologies differ by exactly one `iptables` flag and produce
opposite readings on the same counter, which is a comparison rather than two
unrelated green ticks.

## Privilege

Building a topology needs `CAP_NET_ADMIN`. The lab does not ask you to be root
to get it: an unprivileged user namespace grants full capabilities *inside
itself*, and a network namespace created within one accepts every `ip`,
`iptables` and `tc` command here.

So there are three cases and the lab distinguishes them:

1. the capability is already held — used directly;
2. a user namespace can be obtained — the pytest session re-executes itself
   inside `unshare -U -r -n -m` and carries on, which is the ordinary case;
3. neither — every test is **skipped**, with a message saying what was tried,
   what it said, and which kernel knob is refusing.

Case 3 is common rather than exotic, and telling 2 from 3 is done by *attempting*
a namespace rather than by reading the sysctls that are supposed to predict one.
That matters because the re-execution is an `execvp`: a wrong "yes" replaces the
pytest process, and the run ends with `unshare`'s one-line complaint instead of a
test report.

**On Ubuntu 24.04 and later the lab skips by default.**
`kernel.apparmor_restrict_unprivileged_userns` is 1 out of the box and refuses
the `uid_map` write, while `unprivileged_userns_clone` and
`max_user_namespaces` both read permissive. To run the lab there, either:

```bash
sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0   # revert with =1
sudo -E python -m pytest                                        # or just be root
```

Neither is persistent, and neither is required — skipping is a correct outcome.

## What this lab does not prove

Read this before quoting a result from it. In the spirit of the same section in
the peer-to-peer plan, and for the same reason: a richer environment makes it
easier, not harder, to believe a test proved something it did not.

- **The UDP block is not what causes the fallback.** The `udp_blocked` topology
  runs no rendezvous host, so the direct path cannot be set up whether or not
  the router forwards UDP — removing the `iptables` rule leaves that test
  passing. What it shows is that the fallback fires, the transfer completes
  across a routed network, and the carrier is reported. The rule is there so the
  network matches the shape being described, not because any assertion can
  attribute anything to it.

  This was once believed unfixable, and is no longer: a rendezvous host makes
  the direct path able to *succeed*, which is what attributing a failure to a
  cause requires. Giving `udp_blocked` one would turn it into a real
  attribution test. Not done yet — it is listed under the plan's Phase 5, where
  negative controls get recorded.
- **A netns NAT is not carrier-grade NAT.** `iptables` MASQUERADE over
  `nf_conntrack` models a home router. It has neither the mapping timeouts, the
  port exhaustion, nor the behaviour of a real appliance, and no result here
  transfers to one.
- **This is not the mainline DHT.** The direct-path topologies run a three-node
  `mainline` testnet that knows only itself. It answers the same queries over the
  same wire protocol and has none of the real network's size, churn, latency or
  hostile participants. A rendezvous timing measured here says nothing about
  rendezvous timing in the field.
- **This is not n0's relay infrastructure**, its regions, or its capacity. The
  lab's relay is the real `iroh-relay` server, serving plain HTTP on one host.
- **The lab's address discovery runs on a self-signed certificate.** QUIC
  address discovery needs TLS and there is no CA that will certify a namespace
  address, so the helper issues its own naming the bind address. That works
  because iroh does not verify relays against a public root — it authenticates
  peers by endpoint id — but it is not the certificate path a production relay
  uses, and nothing here exercises that path.
- **A punch measured here is a punch against this model.** Full-cone means
  "port-preserving `MASQUERADE` over `nf_conntrack`", and symmetric means
  "`--random-fully`". Both are measured rather than assumed, and neither is an
  appliance.
- **`netem` is not a real network.** It produces uniform delay and independent
  uniform loss. Real links produce bursts, reordering, bufferbloat and
  asymmetry; surviving 1% independent loss is not surviving 1% bursty loss.
- **No IPv6, no dual-stack**, so no address-family selection.
- **No browser.** The web client needs the relay and has no namespace story.
- **Throughput numbers are veth numbers.** What the latency lane tests is the
  *shape* of the relationship between the window and the round-trip time, which
  is a property of Drop's flow control. It is not a measurement of any real
  link and must never be quoted as one.
- **The latency lane does not explain the factor of two.** Every measured rate
  lands at 41-53% of `WINDOW_BYTES / RTT`, consistently. The window is being
  enforced and it does scale with the round trip; what costs the other half is
  not established. A sender that fills its 16 MiB window and then waits on a
  4 MiB acknowledgement batch would produce this shape, and that is a guess.
  The ceiling is asserted one-sidedly so this gap cannot quietly become the
  thing under test.
- **The loss lane does not model a congested link.** `netem` drops
  independently at a uniform rate; a real link under congestion drops tail
  packets together. And 1% a hop is not 1% end to end — a chunk crosses the
  router twice on its way from sender to relay to receiver, so about 2% of them
  meet a drop.
- **Nothing here checks the security properties.** Not the encryption, not the
  one-guess policy, not the address filter. Those are covered in Rust, and a
  topology adds nothing to them.

## Rules this lab follows

- **No part of the Drop protocol is implemented here.** No envelope, no
  handshake, no framing, no chunk sealing.
  [`decisions.md`](../docs/decisions.md) entry 11 refuses a second
  implementation of the envelope for the browser; a Python one would
  reintroduce the same drift. The lab starts real binaries and inspects what
  comes out — a checksum, an exit code, and the line `drop --status` prints.
- **It matches the status line, not the prose.** `drop-status: path=relay
  fallback=rendezvous` is stable; the sentences above it are written to be
  reworded.
- **The relay gets no test-only configuration.** The bounds in
  [`src/config.rs`](../src/config.rs) are compile-time on purpose, and a
  topology that wants a different limit does not get one.
- **Every topology has a negative control.** `test_the_topology_is_load_bearing`
  shows that the router is really carrying the transfer and that the relay is
  really the thing relaying it. The latency lane measures the unimpaired link
  first and refuses to continue unless it is faster than every ceiling it is
  about to assert, since a rate under a ceiling otherwise says only that the
  lab is slow. The loss lane reads the qdisc back, because at zero RTT a 1%
  drop rate completes as fast as no loss at all and timing would not notice a
  `tc` command that never took effect. A lab that cannot fail is measuring
  nothing.
- **An impaired network is measured, never assumed.** The latency lane pings
  the network it built and fails if the round trip is not the one it asked
  for. Dividing a byte count by a requested RTT would let a misbuilt topology
  produce a number that looked like a finding.
- **Synthetic fixtures only.** Payloads are generated, incompressible, and
  reproducible from a seed.
- **It never blocks pull-request CI.** Runs take minutes and need a namespace.
