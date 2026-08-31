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

from dataclasses import dataclass

from netns import Lab

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
    #: Filled in by topologies that inject delay, after measuring it.
    rtt_ms: float | None = None

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

    relay = None
    if with_relay:
        relay = lab.add("relay")
        to_relay = lab.connect(router, relay, RELAY_SUBNET)
        lab.route_default(relay, to_relay.left_address)

    return Net(
        lab=lab,
        sender=sender,
        receiver=receiver,
        router=router,
        relay=relay,
        name="base",
        proves="nothing on its own",
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
