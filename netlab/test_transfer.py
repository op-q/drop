"""Transfers across constructed networks.

Each test builds a topology, runs the real binaries in it, and asserts four
things where they apply: the file arrived byte-identical, which carrier moved
it, whether a fallback fired, and how fast it went.

Nothing here reimplements any part of Drop. The assertions are over a checksum,
an exit code, and the machine-readable line `drop --status` prints.
"""

from __future__ import annotations

import pytest

import netns
import runner
import topologies

#: Small enough to keep a plumbing test quick, large enough to cross several
#: chunks and exercise the acknowledgement path rather than a single write.
PAYLOAD_BYTES = 4 * 1024 * 1024


def test_a_relayed_transfer_crosses_a_routed_network(lab, binaries, workspace):
    """The control: a transfer over two hops, with nothing impaired.

    Loopback has neither a router nor a second segment, so nothing in the Rust
    suite has moved a Drop transfer across one. This is the smallest claim the
    lab makes and everything below it assumes this works.
    """
    net = topologies.routed_lan(lab)
    source = workspace / "payload.bin"
    destination = workspace / "received"
    runner.synthetic_payload(source, PAYLOAD_BYTES)

    with runner.Relay(binaries, net):
        result = runner.transfer(binaries, net, source, destination, transport="relay")

    assert result.ok, result.why_it_failed()
    assert result.sender.carrier == "relay"
    assert result.receiver.carrier == "relay"

    # Asked for, not fallen back to. Reporting `rendezvous` here would be the
    # line describing a failure that did not happen.
    assert result.sender.fallback == "none"
    assert result.receiver.fallback == "none"

    arrived = destination / "payload.bin"
    assert arrived.is_file(), f"nothing arrived: {sorted(destination.iterdir())}"
    assert runner.sha256(arrived) == runner.sha256(source)


def test_a_udp_blocked_network_falls_back_to_the_relay_and_says_so(
    lab, binaries, workspace
):
    """`auto` on a network where the direct path cannot be set up.

    The transfer must complete rather than fail, and must *say* it fell back —
    that reporting is what makes a slow transfer diagnosable, and it is the
    half of the peer-to-peer plan's gate that survives having no internet.

    **What this does not show**, and the README says it at more length: that the
    UDP block is what caused the fallback. This lab has no route to the
    internet, so the direct path cannot be set up whether or not UDP is
    forwarded, and removing the rule would leave this test passing. The block
    is here so the *network* matches the shape being described, not because the
    assertion can attribute anything to it.
    """
    net = topologies.udp_blocked(lab)
    source = workspace / "payload.bin"
    destination = workspace / "received"
    runner.synthetic_payload(source, PAYLOAD_BYTES)

    with runner.Relay(binaries, net):
        result = runner.transfer(binaries, net, source, destination, transport="auto")

    assert result.ok, result.why_it_failed()

    # The sender's reason is the deterministic one: it cannot reach a home
    # relay, so becoming reachable times out and rendezvous is what failed.
    assert result.sender.carrier == "relay"
    assert result.sender.fallback == "rendezvous"

    # The receiver's reason is not pinned. It either resolves nothing under the
    # nameplate (`no-record`) or cannot reach the directory at all
    # (`rendezvous`), and which one depends on how the DHT client fails with no
    # route — a third party's behaviour, not Drop's.
    assert result.receiver.carrier == "relay"
    assert result.receiver.fallback in {"rendezvous", "no-record"}

    arrived = destination / "payload.bin"
    assert arrived.is_file(), f"nothing arrived: {sorted(destination.iterdir())}"
    assert runner.sha256(arrived) == runner.sha256(source)


def test_the_topology_is_load_bearing(lab, binaries, workspace):
    """The negative controls, without which nothing above means anything.

    A lab that cannot fail is measuring nothing, and a topology that was never
    really assembled would let every test here pass while the two ends sat on
    the same network. Two things are checked, and they fail for different
    reasons on purpose:

    - with the router not forwarding, the ends cannot reach each other at all,
      so the namespaces really are separated by it;
    - with no relay process running, a relayed transfer fails, so the relay
      really is what carries the bytes rather than something incidental.

    Deliberately *not* checked: that blocking UDP causes the fallback. It does
    not here — see the topology's own docstring and `README.md`.
    """
    net = topologies.routed_lan(lab)
    source = workspace / "payload.bin"
    destination = workspace / "received"
    runner.synthetic_payload(source, 64 * 1024)

    # A relay that is running and reachable, so what follows is about the
    # network rather than about a server that never started.
    with runner.Relay(binaries, net):
        assert lab.reaches(net.sender, topologies.RELAY_ADDRESS)
        runner.run(["sysctl", "-w", "net.ipv4.ip_forward=0"], netns=net.router)
        assert not lab.reaches(
            net.sender, topologies.RELAY_ADDRESS
        ), "the ends reach each other without the router, so they are not really apart"
        runner.run(["sysctl", "-w", "net.ipv4.ip_forward=1"], netns=net.router)
        assert lab.reaches(net.sender, topologies.RELAY_ADDRESS)

    # The relay has now stopped. Nothing else changed, so a transfer that still
    # succeeds was never using it.
    result = runner.transfer(
        binaries, net, source, destination, transport="relay", timeout=60.0
    )

    assert not result.ok, (
        "a relayed transfer succeeded with no relay running, so these tests "
        "are not measuring what they claim"
    )
    assert not (destination / "payload.bin").exists()


#: `WINDOW_BYTES` in `cli/src/send.rs`, and the 16 MiB in `docs/protocol.md`
#: line 356. Duplicated here on purpose: if the sender's window changes and
#: nobody updates this, the ceiling assertion below fails and says so, which is
#: the notification this lane exists to give.
WINDOW_MIB = 16

#: The acknowledgement loops to measure at. Chosen so the window is the binding
#: constraint rather than the pipeline: the ceilings are 80, 40 and 20 MiB/s,
#: and an unimpaired run through this lab reaches several hundred. The control
#: below refuses to trust that and measures it.
ACK_LOOPS_MS = (200.0, 400.0, 800.0)

#: Payloads for the two-point rate measurement. The gap between them is what is
#: actually timed, so it is the gap and not the sizes that has to be large
#: enough to measure.
SMALL_MIB = 16
LARGE_MIB = 80


def test_the_acknowledgement_window_bounds_throughput_as_latency_grows(
    lab, binaries, workspace, record_measurement
):
    """`docs/protocol.md` line 356, checked against a network that has an RTT.

    The sender keeps at most `WINDOW_BYTES` unacknowledged, so its throughput
    cannot exceed `WINDOW_BYTES / RTT` however much bandwidth exists. Two
    things are asserted and the second is the one that carries the weight:

    - the rate never **exceeds** the ceiling, which would mean the window was
      not being enforced at all;
    - the rate is **inversely proportional** to the round trip — doubling the
      RTT halves it. A ceiling can be satisfied by any slow implementation, and
      the pipeline being slow for an unrelated reason would satisfy it too.
      Only the window produces the relationship.

    The RTT in that formula is the loop an acknowledgement makes, which is four
    traversals rather than two — see `topologies.high_latency`. It is measured
    on the built network rather than taken from what was asked for.
    """
    # The control, and without it the rest is unfalsifiable: every ceiling below
    # has to sit under what this link can do unimpaired, or a rate beneath a
    # ceiling says only that the lab is slow.
    #
    # Timed whole rather than by the slope used below, because here the slope is
    # the wrong instrument. Unimpaired, both payloads land in about a tenth of a
    # second and the difference between them is scheduler noise — a denominator
    # near zero, which is either a wild number or, if the larger run happens to
    # come out faster, no number at all. One transfer's plain rate is a *lower
    # bound* on the streaming rate, since setup can only drag it down, and a
    # lower bound is all a control needs.
    unimpaired = topologies.routed_lan(lab)
    source = workspace / "control" / "payload.bin"
    runner.synthetic_payload(source, LARGE_MIB << 20)

    with runner.Relay(binaries, unimpaired):
        control = runner.transfer(
            binaries, unimpaired, source, workspace / "control" / "received",
            transport="relay",
        )
    assert control.ok, control.why_it_failed()
    record_measurement("unimpaired", f"at least {control.mib_per_second:.0f} MiB/s")

    highest_ceiling = WINDOW_MIB / (min(ACK_LOOPS_MS) / 1000)
    assert control.mib_per_second > highest_ceiling, (
        f"unimpaired this link carries {control.mib_per_second:.1f} MiB/s, which is "
        f"not comfortably above the highest ceiling under test "
        f"({highest_ceiling:.0f} MiB/s). Every assertion below would hold for a "
        f"pipeline with no window at all. Raise the values in ACK_LOOPS_MS."
    )

    # The namespace names are fixed, so the control's network has to go before
    # another can be built.
    lab.teardown()

    measured = []
    for ack_loop_ms in ACK_LOOPS_MS:
        with netns.Lab() as impaired:
            net = topologies.high_latency(impaired, ack_loop_ms)

            # The network that exists, not the one that was requested. Dividing
            # by an assumed RTT would let a misbuilt topology produce a number
            # that looked like a finding.
            achieved = topologies.measure_ack_loop(net)
            assert abs(achieved - ack_loop_ms) < 0.1 * ack_loop_ms, (
                f"asked for a {ack_loop_ms:.0f}ms acknowledgement loop and built "
                f"a {achieved:.0f}ms one; this is not the network under test"
            )

            rate = runner.measure_streaming_rate(
                binaries,
                net,
                workspace / f"latency-{ack_loop_ms:.0f}",
                SMALL_MIB << 20,
                LARGE_MIB << 20,
            )

        ceiling = WINDOW_MIB / (achieved / 1000)
        record_measurement(
            f"ack loop {ack_loop_ms:.0f}ms",
            f"measured {achieved:.1f}ms, ceiling {ceiling:.1f} MiB/s, {rate}",
        )

        assert rate.mib_per_second <= ceiling, (
            f"streamed {rate.mib_per_second:.1f} MiB/s over a {achieved:.0f}ms "
            f"acknowledgement loop, above the {ceiling:.1f} MiB/s that a "
            f"{WINDOW_MIB} MiB window allows. Either the window is not being "
            f"enforced or it is larger than {WINDOW_MIB} MiB."
        )
        measured.append((achieved, rate))

    # Doubling the round trip should halve the rate. The tolerance is wide
    # because a veth pair, a relay process and a scheduler are all in the path;
    # it is nowhere near wide enough to admit a rate that ignores the RTT,
    # which would give a ratio of 1.
    for (slower_ms, slower), (faster_ms, faster) in zip(measured, measured[1:]):
        expected = faster_ms / slower_ms
        observed = slower.mib_per_second / faster.mib_per_second
        assert abs(observed - expected) < 0.35 * expected, (
            f"the round trip grew {expected:.1f}x from {slower_ms:.0f}ms to "
            f"{faster_ms:.0f}ms, so the rate should have fallen by about the "
            f"same factor; it changed by {observed:.2f}x "
            f"({slower.mib_per_second:.1f} to {faster.mib_per_second:.1f} MiB/s)"
        )


#: Large enough that loss is certain rather than probable: at 1% a hop, a
#: 16 MiB payload is thousands of frames and a run where nothing was dropped
#: would be a lottery win rather than a pass.
LOSSY_PAYLOAD_MIB = 16

#: What "did not hang" means. Generous against the unimpaired figure — this is
#: not a throughput bound, and asserting one under random loss would be asking
#: for a flake. It is the line between "slow" and "never".
LOSSY_DEADLINE_SECONDS = 120.0

#: The harsher run. Recorded, never asserted on: at this rate the outcome is
#: genuinely probabilistic, and turning it into a gate would buy a flaky test
#: in exchange for nothing the 1% run does not already assert.
HARSH_LOSS_PERCENT = 5.0


def _outcome(result: runner.Transfer) -> str:
    """One line saying what a transfer did, for the measured table.

    Kept off `Transfer` because it phrases a deadline in this lane's terms, and
    because `mib_per_second` refuses — correctly — to divide by a run that
    never finished.
    """
    if result.timed_out:
        return f"did not finish within {LOSSY_DEADLINE_SECONDS:.0f}s"
    if not result.ok:
        return f"failed after {result.seconds:.1f}s"
    return f"completed in {result.seconds:.1f}s ({result.mib_per_second:.1f} MiB/s)"


def test_a_lossy_network_delivers_the_file_intact_and_terminates(
    lab, binaries, workspace, record_measurement
):
    """1% loss a hop, and the second assertion is the one worth having.

    That the bytes arrive uncorrupted is table stakes — every layer underneath
    has a checksum and a retransmit, and a mismatch here would mean something
    far more alarming than packet loss.

    The interesting failure is a transfer that **never finishes and never
    errors**: a chunk dropped at the wrong moment, an acknowledgement that
    releases nothing, and two processes waiting on each other until somebody
    notices. Loopback cannot produce that because loopback does not drop
    packets. So the deadline is the assertion, and `Transfer.timed_out` exists
    so that hitting it is reported as this transfer's outcome rather than as a
    broken lab.
    """
    net = topologies.lossy(lab, 1.0)
    source = workspace / "payload.bin"
    destination = workspace / "received"
    runner.synthetic_payload(source, LOSSY_PAYLOAD_MIB << 20)

    # The topology, shown to be load-bearing before anything is concluded from
    # it. A 16 MiB payload crossing an unimpaired veth pair takes about a tenth
    # of a second, and it takes about a tenth of a second here too — so "it was
    # fast" is no evidence either way, and the drops have to be demonstrated
    # rather than inferred from the timing.
    for interface in net.router_interfaces.values():
        configured = lab.netem_on(net.router, interface)
        assert "loss 1%" in configured, (
            f"{interface} was asked for 1% loss and reports {configured!r}; "
            "this network is not the one under test"
        )

    record_measurement(
        "1% loss, observed",
        f"{lab.measure_loss(net.sender, topologies.RELAY_ADDRESS):.1f}% of probes "
        "lost sender to relay (two hops)",
    )

    with runner.Relay(binaries, net):
        result = runner.transfer(
            binaries,
            net,
            source,
            destination,
            transport="relay",
            timeout=LOSSY_DEADLINE_SECONDS,
        )

        record_measurement("1% loss a hop", _outcome(result))

        assert result.ok, result.why_it_failed()
        assert not result.timed_out, (
            f"the transfer was still running after {LOSSY_DEADLINE_SECONDS:.0f}s. "
            "Under loss that is the shape of an acknowledgement that never "
            "released the window, not of a slow network."
        )

        arrived = destination / "payload.bin"
        assert arrived.is_file(), f"nothing arrived: {sorted(destination.iterdir())}"
        assert runner.sha256(arrived) == runner.sha256(source)

        # Harsher, and deliberately not asserted on. `tc` accepts the change in
        # place, so this is the same network with one number altered.
        for interface in net.router_interfaces.values():
            lab.loss(net.router, interface, HARSH_LOSS_PERCENT)

        harsh = runner.transfer(
            binaries,
            net,
            source,
            workspace / "received-harsh",
            transport="relay",
            timeout=LOSSY_DEADLINE_SECONDS,
        )

    outcome = _outcome(harsh)
    if harsh.ok:
        arrived_harsh = workspace / "received-harsh" / "payload.bin"
        intact = arrived_harsh.is_file() and runner.sha256(arrived_harsh) == runner.sha256(source)
        outcome += ", intact" if intact else ", CORRUPTED"

    record_measurement(f"{HARSH_LOSS_PERCENT:g}% loss a hop", f"{outcome} — recorded, not asserted")


# ---------------------------------------------------------------------------
# The direct path.
#
# These three are the topologies that were blocked on open question 1 of the
# plan. What unblocked them is `DROP_RENDEZVOUS_RELAY` and
# `DROP_RENDEZVOUS_BOOTSTRAP`, which point the direct path at infrastructure a
# deployment runs itself — see
# `docs/plans/self-hosted-rendezvous-plan-2026-09-10.md` for why that is a
# deployment feature rather than a knob added for these tests.
#
# All three run `--transport p2p`, which forbids falling back. That is what
# makes them falsifiable: with `auto`, a rendezvous that quietly failed would
# still produce a completed transfer, and the assertion would be satisfied by
# the thing it was written to rule out. With `p2p` a failure is a failure.
# ---------------------------------------------------------------------------

#: Thresholds as fractions of the payload, not absolute byte counts, and the
#: gap between them is deliberate.
#:
#: A relayed transfer crosses the rendezvous link **twice** — in on one
#: direction, out on the other — so the summed counter reads about 2x the
#: payload. A punched one carries DISCO traffic, DHT queries, and whatever went
#: over the relay in the moments before a direct path was validated. That last
#: term is why an absolute ceiling in kilobytes would be wrong: on a veth link
#: a few hundred milliseconds of relaying is megabytes.
#:
#: So: below half the payload is a punch, above the whole payload is a relay,
#: and the factor of two between them is the room this measurement has. A run
#: landing between them is not a marginal pass to be tuned away — it means the
#: connection migrated late, and the numbers are recorded so that is visible.
PUNCHED_CEILING = 0.5
RELAYED_FLOOR = 1.0

#: Large enough that a payload crossing the rendezvous link is unmistakable
#: against the signalling that always crosses it.
DIRECT_PAYLOAD_BYTES = 16 * 1024 * 1024


def _link_report(carried: int) -> str:
    """What the rendezvous link carried, as MiB and as a share of the payload.

    Both, because the ratio is what the assertions use and the absolute figure
    is what a reader needs to sanity-check it against a topology.
    """
    return (
        f"{carried / 1024 / 1024:.2f} MiB, "
        f"{carried / DIRECT_PAYLOAD_BYTES:.2f}x the "
        f"{DIRECT_PAYLOAD_BYTES / 1024 / 1024:.0f} MiB payload"
    )


def _direct_transfer(binaries, rendezvous_binary, net, workspace, size=DIRECT_PAYLOAD_BYTES):
    """Runs one direct transfer and reports what the rendezvous link carried.

    The byte count is taken across the transfer rather than absolutely, because
    the link has already carried the helper's own startup chatter by the time a
    transfer begins.
    """
    source = workspace / "payload.bin"
    destination = workspace / "received"
    runner.synthetic_payload(source, size)

    with runner.Rendezvous(rendezvous_binary, net) as where:
        before = net.lab.interface_bytes(net.router, net.rendezvous_interface)
        result = runner.transfer(
            binaries, net, source, destination, transport="p2p", rendezvous=where
        )
        carried = net.lab.interface_bytes(net.router, net.rendezvous_interface) - before

    return result, carried, source, destination


def test_a_plain_lan_transfers_with_no_drop_process_anywhere(
    lab, binaries, rendezvous_binary, workspace, record_measurement
):
    """The peer-to-peer plan's first validation gate, finally runnable.

    Two hosts, a router that translates nothing, and **no Drop server in
    existence** — checked with `pgrep` rather than inferred from this test not
    having started one, because "no Drop server was involved" is the strongest
    claim the lab makes and it should rest on a measurement.

    An iroh relay does run, in its own namespace, and that is not a hedge: the
    sender has nothing publishable without one. `publishable` strips every
    private address from a record (`docs/decisions.md` entry 14) and every
    address here is private, so a record naming the relay is the only record
    that can exist. Production's direct path uses n0's relay for exactly this,
    so what runs here is the shipped shape with the third party moved inside the
    lab. The claim being tested — no *Drop-operated* server — is untouched by it.
    """
    net = topologies.plain_lan(lab)

    assert not runner.relay_is_running(), (
        "an `api` process is running, so this topology cannot claim what it exists to claim"
    )

    result, carried, source, destination = _direct_transfer(
        binaries, rendezvous_binary, net, workspace
    )

    record_measurement(
        "plain LAN, rendezvous link carried",
        _link_report(carried),
    )

    assert result.ok, result.why_it_failed()
    assert result.sender.carrier == "p2p"
    assert result.receiver.carrier == "p2p"
    assert result.sender.fallback == "none"
    assert result.receiver.fallback == "none"

    arrived = destination / "payload.bin"
    assert arrived.is_file(), f"nothing arrived: {sorted(destination.iterdir())}"
    assert runner.sha256(arrived) == runner.sha256(source)

    # Still true after the transfer, not only before it. A relay started
    # mid-run would be as fatal to the claim as one started early.
    assert not runner.relay_is_running(), "an `api` process appeared during the transfer"

    assert carried < PUNCHED_CEILING * DIRECT_PAYLOAD_BYTES, (
        f"the rendezvous link carried {_link_report(carried)}, which is the payload rather "
        "than signalling — the peers met through the relay instead of over the LAN"
    )


def test_a_full_cone_nat_is_punched_through(
    lab, binaries, rendezvous_binary, workspace, record_measurement
):
    """Both peers behind a port-preserving NAT still reach each other directly.

    **The mapping is measured before the transfer, not assumed from the
    `iptables` rule.** The plan's risk list is explicit that getting this wrong
    produces a test that passes for the wrong reason, and the reason it would
    pass is that every one of these assertions is also satisfied by a NAT that
    behaves differently than intended. So the first thing this test does is send
    one socket's datagrams to two destinations and compare the source ports the
    far end saw.

    **The punch itself is measured on the wire**, because `drop --status` cannot
    report it: `path=p2p` means "no Drop server", and iroh carries the same QUIC
    connection over its own relay when a punch fails. `cli/src/direct.rs` says
    so directly. Only the byte counters distinguish the two.
    """
    net = topologies.full_cone_nat(lab)

    first, second = topologies.measure_nat_mapping(net)
    record_measurement(
        "full-cone NAT, source port seen by two destinations", f"{first} and {second}"
    )
    assert first == second, (
        f"two destinations saw ports {first} and {second}, so this NAT's mapping depends on "
        "the destination. That is a symmetric NAT, and this topology claims to be full cone"
    )

    result, carried, source, destination = _direct_transfer(
        binaries, rendezvous_binary, net, workspace
    )

    record_measurement(
        "full-cone NAT, rendezvous link carried",
        _link_report(carried),
    )

    assert result.ok, result.why_it_failed()
    assert result.sender.carrier == "p2p"
    assert result.receiver.carrier == "p2p"

    arrived = destination / "payload.bin"
    assert arrived.is_file(), f"nothing arrived: {sorted(destination.iterdir())}"
    assert runner.sha256(arrived) == runner.sha256(source)

    assert carried < PUNCHED_CEILING * DIRECT_PAYLOAD_BYTES, (
        f"the rendezvous link carried {_link_report(carried)}, so the connection stayed on "
        "the relay. The NAT was measured punchable, so this is iroh not punching it"
    )


def test_a_symmetric_nat_defeats_the_punch_and_the_transfer_survives(
    lab, binaries, rendezvous_binary, workspace, record_measurement
):
    """A punch that cannot succeed, and a transfer that completes anyway.

    **The plan asked for the wrong assertion here**, and it is worth stating
    because the wrong one fails and would invite somebody to weaken something.
    It said to expect `fallback=rendezvous` or `no-record` — the Drop relay
    taking over. That is not what happens. Rendezvous succeeds: the DHT is
    reachable and the record names a relay. The QUIC connection succeeds too,
    carried over the iroh relay. The Drop relay is never consulted, and there is
    no `api` process here for it to be consulted at all.

    So what is asserted is the shape that actually occurs: `path=p2p
    fallback=none`, the file intact, and the **payload visibly crossing the
    rendezvous link** — which is the evidence that the punch failed, and the
    exact inverse of the full-cone row. Two topologies differing in one
    `iptables` flag produce opposite measurements on the same counter.
    """
    net = topologies.symmetric_nat(lab)

    first, second = topologies.measure_nat_mapping(net)
    record_measurement(
        "symmetric NAT, source port seen by two destinations", f"{first} and {second}"
    )
    assert first != second, (
        f"both destinations saw port {first}, so this NAT's mapping is endpoint-independent "
        "and a punch through it can succeed. This topology is a full-cone one wearing a "
        "symmetric label, and every assertion below would pass for the wrong reason"
    )

    result, carried, source, destination = _direct_transfer(
        binaries, rendezvous_binary, net, workspace
    )

    record_measurement(
        "symmetric NAT, rendezvous link carried",
        _link_report(carried),
    )

    assert result.ok, result.why_it_failed()

    # No Drop relay was consulted, because a failed punch is not a failed
    # connection and there is no `api` process in this topology at all.
    assert result.sender.carrier == "p2p"
    assert result.receiver.carrier == "p2p"
    assert result.sender.fallback == "none"
    assert result.receiver.fallback == "none"
    assert not runner.relay_is_running()

    arrived = destination / "payload.bin"
    assert arrived.is_file(), f"nothing arrived: {sorted(destination.iterdir())}"
    assert runner.sha256(arrived) == runner.sha256(source)

    assert carried > RELAYED_FLOOR * DIRECT_PAYLOAD_BYTES, (
        f"the rendezvous link carried only {_link_report(carried)}, so the payload went "
        "directly. A punch through a measured-symmetric NAT should not have succeeded, and "
        "if it did then the mapping probe and the transfer disagree about this network"
    )
