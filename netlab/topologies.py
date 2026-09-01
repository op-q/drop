"""The named network shapes a run can be given.

Each function here takes a `Lab`, builds one network in it, and returns a `Net`
saying where things live. A topology is a function, not a class: there is one
implementation of each and nothing to configure beyond its arguments, so an
abstraction over them would be a base class with a single subclass.

# The shape they all share

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

The relay gets its own namespace rather than sharing the router's. Putting a
Drop server inside the router would place it *inside* the NAT boundary under
test, which is the opposite of the deployment being modelled — and it would
make a "no relay in the path" topology impossible to state honestly, because
the relay would be the path.

# Addressing

Every address is inside `10.0.0.0/8`. That is deliberate and it is not only
about privacy: `rendezvous::publishable` refuses to put a private address into
a published record, so no address here can be mistaken for one Drop would
publish, and no report can quote something that looks like a real host.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from netns import Lab, LabError

SENDER_SUBNET = "10.10.0.0"
RECEIVER_SUBNET = "10.20.0.0"
RELAY_SUBNET = "10.30.0.0"

RELAY_ADDRESS = "10.30.0.2"
RELAY_PORT = 8080


@dataclass
class Net:
    """A built network, and what a runner needs to know about it."""

    lab: Lab
    sender: str
    receiver: str
    router: str
    #: `None` when the topology deliberately runs no Drop server at all.
    relay: str | None
    name: str
    #: What this topology is meant to demonstrate, carried into the report so a
    #: reader does not have to infer it from the name.
    proves: str
    #: The router's own interface facing each namespace, keyed by that
    #: namespace's name. An impairment is applied to an interface rather than
    #: to a host, so a topology that shapes the network has to name one, and
    #: deriving the name at the call site would duplicate `netns`'s convention.
    router_interfaces: dict[str, str] = field(default_factory=dict)

    @property
    def relay_url(self) -> str | None:
        if self.relay is None:
            return None
        return f"http://{RELAY_ADDRESS}:{RELAY_PORT}"


def _base(lab: Lab, *, with_relay: bool) -> Net:
    """Sender and receiver on separate segments, routed through a middle box."""
    sender = lab.add("sender")
    receiver = lab.add("receiver")
    router = lab.add("router")

    to_sender = lab.connect(router, sender, SENDER_SUBNET)
    to_receiver = lab.connect(router, receiver, RECEIVER_SUBNET)

    lab.route_default(sender, to_sender.left_address)
    lab.route_default(receiver, to_receiver.left_address)
    lab.forward(router)

    interfaces = {
        sender: to_sender.left_interface,
        receiver: to_receiver.left_interface,
    }

    relay = None
    if with_relay:
        relay = lab.add("relay")
        to_relay = lab.connect(router, relay, RELAY_SUBNET)
        lab.route_default(relay, to_relay.left_address)
        interfaces[relay] = to_relay.left_interface

    return Net(
        lab=lab,
        sender=sender,
        receiver=receiver,
        router=router,
        relay=relay,
        name="base",
        proves="nothing on its own",
        router_interfaces=interfaces,
    )


def routed_lan(lab: Lab) -> Net:
    """Two hosts, two segments, a router, and a relay. No impairment.

    The control for every other topology: whatever a topology below shows, this
    one shows what the same transfer does without it.
    """
    net = _base(lab, with_relay=True)
    net.name = "routed-lan"
    net.proves = "a transfer completes across a routed network"
    return net


def udp_blocked(lab: Lab) -> Net:
    """A router that will not forward UDP.

    The shape of a corporate network that permits TCP and nothing else, and the
    condition under which a direct transfer must fall back to the relay rather
    than failing.

    **Read `README.md` before trusting this one.** In a lab with no route to the
    internet the direct path cannot succeed whether or not UDP is blocked, so
    this topology cannot attribute the fallback to the block. What it does show
    is that the fallback fires, completes across a routed network, and says so.
    """
    net = _base(lab, with_relay=True)
    lab.drop_udp(net.router)
    net.name = "udp-blocked"
    net.proves = "the transfer falls back to the relay, completes, and reports it"
    return net


def high_latency(lab: Lab, ack_loop_ms: float) -> Net:
    """A router that holds every packet, so the acknowledgement loop is long.

    `ack_loop_ms` is the round trip that *matters*, and it is not the round trip
    between any two hosts. `docs/protocol.md` line 86 has the receiver — not the
    relay — acknowledge bytes, and the relay merely forwards that
    acknowledgement on. So the loop releasing the sender's window is

    ```text
        sender -> relay -> receiver        the chunk
        receiver -> relay -> sender        its acknowledgement
    ```

    which is four traversals, not two. Delay is applied to each of the router's
    three interfaces at a quarter of the target: every one of those traversals
    crosses exactly one of them, in one direction, once.

    That arithmetic is checked rather than trusted, by `measure_ack_loop` below.
    """
    net = _base(lab, with_relay=True)

    for interface in net.router_interfaces.values():
        lab.delay(net.router, interface, ack_loop_ms / 4)

    net.name = f"high-latency-{ack_loop_ms:.0f}ms"
    net.proves = "throughput is bounded by the window and falls as the round trip grows"
    return net


def measure_ack_loop(net: Net) -> float:
    """The round trip an acknowledgement really makes, in milliseconds.

    Measured, never assumed, because dividing a byte count by an RTT that was
    only asked for would turn a misbuilt network into a finding.

    The loop is the sum of two pings, and that identity holds however
    asymmetric the impairment turns out to be:

    ```text
        ping(sender, relay)    = (sender->relay) + (relay->sender)
        ping(receiver, relay)  = (receiver->relay) + (relay->receiver)
    ```

    Between them those are the four one-way traversals a chunk and its
    acknowledgement make, each counted once. No traversal is assumed to cost
    the same as its reverse.
    """
    if net.relay is None:
        raise LabError("the acknowledgement loop runs through a relay, and there is none")

    return net.lab.measure_rtt(net.sender, RELAY_ADDRESS) + net.lab.measure_rtt(
        net.receiver, RELAY_ADDRESS
    )


def lossy(lab: Lab, percent: float) -> Net:
    """A router that drops a fraction of what it forwards, in both directions.

    Applied to each of the router's interfaces, which is what puts loss on both
    directions of the path: a chunk crosses the router twice on its way from
    sender to relay to receiver, and its acknowledgement crosses twice coming
    back, each time leaving by a different interface.

    So `percent` is the chance of losing one *hop*, and an end-to-end direction
    crosses two of them — at 1% each way, about 2% of chunks meet a drop
    somewhere. The number is quoted per hop rather than end to end because that
    is what `tc` was told, and a report should be able to name the setting.

    Loss is uniform and independent. Real loss arrives in bursts, and a
    congested link drops tail packets together rather than at random; this
    models neither. See `README.md`.
    """
    net = _base(lab, with_relay=True)

    for interface in net.router_interfaces.values():
        lab.loss(net.router, interface, percent)

    net.name = f"lossy-{percent:g}pc"
    net.proves = "a lossy path still delivers the file intact, and terminates"
    return net
