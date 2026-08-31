"""Transfers across constructed networks.

Each test builds a topology, runs the real binaries in it, and asserts four
things where they apply: the file arrived byte-identical, which carrier moved
it, whether a fallback fired, and how fast it went.

Nothing here reimplements any part of Drop. The assertions are over a checksum,
an exit code, and the machine-readable line `drop --status` prints.
"""

from __future__ import annotations

import pytest

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
