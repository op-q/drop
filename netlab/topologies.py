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

#: The rendezvous host's own link, and the two NAT routers' public links. All
#: three hang off a core router that does no translation, which is what keeps
#: the rendezvous link carrying rendezvous traffic and nothing else — see
#: `Net.rendezvous_interface`.
RENDEZVOUS_SUBNET = "10.40.0.0"
SENDER_PUBLIC_SUBNET = "10.50.0.0"
RECEIVER_PUBLIC_SUBNET = "10.60.0.0"

RENDEZVOUS_ADDRESS = "10.40.0.2"

#: Ports the NAT-mapping probe aims at. Two ports on one host is enough to tell
#: an endpoint-independent mapping from an endpoint-dependent one, because a
#: symmetric NAT keys on the destination *endpoint* and not only its address.
#: High and unregistered, so nothing in the lab can already be listening there.
PROBE_PORTS = (41801, 41802)


@dataclass
class Net:
    """A built network, and what a runner needs to know about it."""

    lab: Lab
    sender: str
    receiver: str
    router: str
    #: `None` when the topology deliberately runs no Drop server at all.
    relay: str | None
    #: Where `netlab-rendezvous` runs — an iroh relay and a small DHT — for the
    #: topologies that exercise the direct path. `None` for the relay
    #: topologies, which need no rendezvous because they never attempt one.
    rendezvous: str | None
    name: str
    #: What this topology is meant to demonstrate, carried into the report so a
    #: reader does not have to infer it from the name.
    proves: str
    #: The router's own interface facing each namespace, keyed by that
    #: namespace's name. An impairment is applied to an interface rather than
    #: to a host, so a topology that shapes the network has to name one, and
    #: deriving the name at the call site would duplicate `netns`'s convention.
    router_interfaces: dict[str, str] = field(default_factory=dict)

    #: The core router's interface facing the rendezvous host, whose byte
    #: counters say whether the rendezvous relay carried the payload or only
    #: introduced the peers. `None` when there is no rendezvous host.
    rendezvous_interface: str | None = None

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
        rendezvous=None,
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


def _direct_base(lab: Lab, *, nat: str | None) -> Net:
    """The shape every direct-path topology shares.

    ```text
      sender ─[10.10]─ nat-a ─[10.50]─┐
                                      ├─ core ─[10.40]─ rendezvous
    receiver ─[10.20]─ nat-b ─[10.60]─┘
    ```

    Six namespaces, and each one is there for a reason the other shapes in this
    file did not need.

    **Two NAT routers, not one.** Hole punching is a property of what happens
    when *both* peers are behind a translation, and a single middle box
    masquerading in both directions does not model it: each end would learn a
    mapping on one of the router's interfaces and have to reach it through
    another, which is a network nobody deploys.

    **A core router that translates nothing.** It joins the two public segments
    and the rendezvous host, so a punched path runs
    `nat-a → core → nat-b` and never touches the rendezvous link.

    **The rendezvous host on its own point-to-point link.** This is the
    load-bearing detail, and it is why the rendezvous host is not simply sat on
    a shared public segment. Its link carries rendezvous traffic *only*, so the
    byte counters on it answer "did the relay carry the payload?" directly. Put
    it on a segment the two NATs share and punched traffic would cross the same
    wire, and the measurement that distinguishes topology 2 from topology 3
    would be measuring both at once.

    `nat` is `None`, `"full-cone"` or `"symmetric"`, and it is the **only**
    difference between the three topologies below. That is deliberate: three
    results are comparable because one variable moved.
    """
    sender = lab.add("sender")
    receiver = lab.add("receiver")
    nat_a = lab.add("nat-a")
    nat_b = lab.add("nat-b")
    core = lab.add("core")
    rendezvous = lab.add("rendezvous")

    to_sender = lab.connect(nat_a, sender, SENDER_SUBNET)
    to_receiver = lab.connect(nat_b, receiver, RECEIVER_SUBNET)
    sender_public = lab.connect(core, nat_a, SENDER_PUBLIC_SUBNET)
    receiver_public = lab.connect(core, nat_b, RECEIVER_PUBLIC_SUBNET)
    to_rendezvous = lab.connect(core, rendezvous, RENDEZVOUS_SUBNET)

    lab.route_default(sender, to_sender.left_address)
    lab.route_default(receiver, to_receiver.left_address)
    lab.route_default(nat_a, sender_public.left_address)
    lab.route_default(nat_b, receiver_public.left_address)
    lab.route_default(rendezvous, to_rendezvous.left_address)

    for router in (nat_a, nat_b, core):
        lab.forward(router)

    if nat is None:
        # A plain LAN is one where the two hosts can actually reach each other,
        # and without these they cannot: the core has no route into either inner
        # segment, so every packet between the ends dies at it.
        #
        # These belong *only* to the untranslated case. Adding them to a NAT
        # topology would leave each peer reachable at the private address it
        # advertises, so the punch would succeed by going around the NAT — and
        # both NAT rows would pass while measuring a routed LAN.
        lab.route(core, SENDER_SUBNET, via=sender_public.right_address)
        lab.route(core, RECEIVER_SUBNET, via=receiver_public.right_address)
    else:
        symmetric = nat == "symmetric"
        lab.masquerade(nat_a, sender_public.right_interface, symmetric=symmetric)
        lab.masquerade(nat_b, receiver_public.right_interface, symmetric=symmetric)

    return Net(
        lab=lab,
        sender=sender,
        receiver=receiver,
        # The core is the router an impairment would be applied to, and the one
        # every path crosses.
        router=core,
        # No Drop relay namespace at all, which is the point of these three and
        # is asserted directly rather than left implicit.
        relay=None,
        rendezvous=rendezvous,
        name="direct-base",
        proves="nothing on its own",
        router_interfaces={
            nat_a: sender_public.left_interface,
            nat_b: receiver_public.left_interface,
            rendezvous: to_rendezvous.left_interface,
        },
        rendezvous_interface=to_rendezvous.left_interface,
    )


def plain_lan(lab: Lab) -> Net:
    """Two routed hosts, no translation anywhere, and no Drop server.

    The topology the peer-to-peer plan's validation lists first, and the
    strongest claim this lab can make: a file crosses with **no Drop process in
    existence**, which `runner.relay_is_running` checks rather than assumes.

    *Not* "no server at all", and the difference is worth stating plainly. An
    iroh relay runs in the `rendezvous` namespace, because without one the
    sender has nothing publishable: `rendezvous::publishable` strips every
    private address from a record — `docs/decisions.md` entry 14 — and a lab
    address is private by construction, so a record naming the relay URL is the
    only record that can be built. Production's direct path uses n0's relay for
    exactly this, so the lab is faithful to it rather than weaker than it. What
    "peer-to-peer" claims, and what is checked here, is that no *Drop-operated*
    server is involved.
    """
    net = _direct_base(lab, nat=None)
    net.name = "plain-lan"
    net.proves = "a transfer completes with no Drop process running anywhere"
    return net


def full_cone_nat(lab: Lab) -> Net:
    """Both peers behind a port-preserving NAT, which a hole can be punched in.

    `MASQUERADE` without randomisation, so a given internal socket is seen at
    the same external port by every destination. The mapping a peer learns
    through the relay is therefore the mapping it can send to, which is the
    whole mechanism hole punching rests on.

    Whether a punch *happened* is not asked of `drop --status`, which reports
    `path=p2p` for any connection with no Drop server in it — relayed or not.
    It is measured on the rendezvous link instead.
    """
    net = _direct_base(lab, nat="full-cone")
    net.name = "full-cone-nat"
    net.proves = "a direct path is established through a port-preserving NAT"
    return net


def symmetric_nat(lab: Lab) -> Net:
    """Both peers behind a NAT whose port depends on where the packet is going.

    `--random-fully`, so each conntrack entry draws a fresh external port and
    the mapping the relay observed is not one the peer can use. Hole punching
    must fail here.

    **What fails is the punch, and not the transfer**, which is where the
    original plan for this row was wrong. `cli/src/direct.rs` is explicit that
    "a failed hole punch is not a failed connection": iroh carries the same QUIC
    connection over its relay, rendezvous succeeded, and the Drop relay is never
    consulted. So the expected outcome is `path=p2p fallback=none` with the
    payload visibly crossing the rendezvous link — not the `fallback=rendezvous`
    the plan first asked for, which would have failed the test and invited
    somebody to weaken it.
    """
    net = _direct_base(lab, nat="symmetric")
    net.name = "symmetric-nat"
    net.proves = "a punch fails, the connection survives over the relay, and the file arrives"
    return net


def measure_nat_mapping(net: Net) -> tuple[int, int]:
    """What two destinations saw as one socket's source port.

    Equal means endpoint-independent and punchable; different means symmetric.
    Measured from the sender through whatever NAT the topology installed, with
    the rendezvous host as the observer because it is the one host both ends can
    reach in every one of these topologies.

    The plan's risk list asks for this before either NAT row is believed, and
    the reason is that believing the `iptables` rule instead is unfalsifiable:
    a symmetric topology that is secretly full-cone still passes, because the
    thing it asserts — that the punch failed — would be produced just as well by
    any other failure.
    """
    if net.rendezvous is None:
        raise LabError("a mapping is measured against a host, and this topology has none")

    return net.lab.observed_source_ports(
        net.sender, net.rendezvous, RENDEZVOUS_ADDRESS, PROBE_PORTS
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
