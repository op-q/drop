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
