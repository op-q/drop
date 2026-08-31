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

```bash
pytest netlab/
```

That is the whole invocation, and it needs no privilege. See
[Privilege](#privilege) for why, and what it does if it cannot get any.

## What is here

```text
netlab/
  netns.py         namespaces, veth pairs, NAT rules, tc netem
  topologies.py    the named network shapes
  runner.py        spawns the real binaries and reads what came out
  conftest.py      builds the binaries; gets a namespace or skips
  test_transfer.py the tests
```

## The network

Four namespaces. All addressing is inside `10.0.0.0/8`, which is both private
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

## Topologies

| Topology | State | What it shows |
| --- | --- | --- |
| `routed_lan` | **runs** | a transfer completes across a router and two segments |
| `udp_blocked` | **runs** | the fallback fires, completes, and reports itself |
| RTT injected | planned | throughput tracks `window / RTT` |
| 1% loss | planned | no corruption, no indefinite hang |
| Full-cone NAT | **blocked** | hole punching succeeds |
| Symmetric NAT | **blocked** | hole punching fails and the fallback is clean |
| Plain LAN, no relay | **blocked** | a transfer with no Drop process anywhere |

The three blocked rows need the direct path, which cannot run in a hermetic lab
as the code stands — see below. It is a decision about production surface, not
missing effort, and it is open question 1 in the plan.

## Privilege

Building a topology needs `CAP_NET_ADMIN`. The lab does not ask you to be root
to get it: an unprivileged user namespace grants full capabilities *inside
itself*, and a network namespace created within one accepts every `ip`,
`iptables` and `tc` command here.

So there are three cases and the lab distinguishes them:

1. the capability is already held — used directly;
2. a user namespace can be obtained — the pytest session re-executes itself
   inside `unshare -Urnm` and carries on, which is the ordinary case;
3. neither — every test is **skipped**, with a message saying which was tried.

Case 3 is real: some kernels set `kernel.unprivileged_userns_clone=0`, and some
container runtimes block the syscall.

## What this lab does not prove

Read this before quoting a result from it. In the spirit of the same section in
the peer-to-peer plan, and for the same reason: a richer environment makes it
easier, not harder, to believe a test proved something it did not.

- **The UDP block is not what causes the fallback.** This is the sharpest one,
  and it is not a defect that can be fixed by trying harder. The lab has no
  route to the internet, so the direct path cannot be set up whether or not the
  router forwards UDP — removing the `iptables` rule leaves
  `udp_blocked` passing. What that topology actually shows is that the fallback
  fires, the transfer completes across a routed network, and the carrier is
  reported. The rule is there so the network matches the shape being described,
  not because any assertion can attribute anything to it. A test that could
  attribute it needs the direct path to be able to succeed, which is the same
  blocker as the three rows above.
- **A netns NAT is not carrier-grade NAT.** `iptables` MASQUERADE over
  `nf_conntrack` models a home router. It has neither the mapping timeouts, the
  port exhaustion, nor the behaviour of a real appliance, and no result here
  transfers to one.
- **This is not the mainline DHT.** Nothing in this lab touches it. Rendezvous
  timing, churn, and hostile participants are all absent.
- **This is not n0's relay infrastructure**, its regions, or its capacity.
- **`netem` is not a real network.** It produces uniform delay and independent
  uniform loss. Real links produce bursts, reordering, bufferbloat and
  asymmetry; surviving 1% independent loss is not surviving 1% bursty loss.
- **No IPv6, no dual-stack**, so no address-family selection.
- **No browser.** The web client needs the relay and has no namespace story.
- **Throughput numbers are veth numbers.** What the latency lane tests is the
  *shape* of the relationship between the window and the round-trip time, which
  is a property of Drop's flow control. It is not a measurement of any real
  link and must never be quoted as one.
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
  really the thing relaying it. A lab that cannot fail is measuring nothing.
- **Synthetic fixtures only.** Payloads are generated, incompressible, and
  reproducible from a seed.
- **It never blocks pull-request CI.** Runs take minutes and need a namespace.
