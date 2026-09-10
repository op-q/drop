"""Linux network namespaces, veth pairs, NAT rules, and link impairments.

Everything here drives `ip`, `iptables` and `tc` through `subprocess` and reads
their exit codes. Nothing in this file parses a packet, speaks a protocol, or
knows anything about Drop. That separation is the point: the transport logic
lives in Rust, and this module owns only the shape of the network it runs on.

# Privilege

The lab needs `CAP_NET_ADMIN`, and the usual way to get it is to be root. It is
not the only way: an unprivileged user namespace grants full capabilities
*inside itself*, and a network namespace created within one accepts every
command below. So the lab re-executes itself into `unshare -Urnm` rather than
asking for privilege it does not need — see `conftest.py`, which does the
re-execution, and `README.md` for what that does and does not isolate.

# Names

Namespaces are created inside a private tmpfs mounted over the namespace
directory, so their names cannot collide with anything on the host and are
plain words rather than generated identifiers. Interface names are derived from
the namespace on the other end, so `to-router` inside `sender` is unambiguous
while reading `ip addr` output during a failure.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import signal
import subprocess
import sys
from dataclasses import dataclass, field

#: Bit position of `CAP_NET_ADMIN` in a capability mask. From
#: `include/uapi/linux/capability.h`; there is no interface that reports it by
#: name, so it is spelled out here with the reference rather than computed.
CAP_NET_ADMIN = 12

#: Where `ip netns` looks for named namespaces. Hard-coded in iproute2, so the
#: lab has to work with this path rather than choose its own.
NETNS_DIR = "/run/netns"

#: Everything the lab shells out to. Checked once, up front, because a missing
#: `tc` should be one clear message and not a failure three topologies in.
REQUIRED_TOOLS = ("ip", "iptables", "tc", "unshare")

#: User, network and mount namespaces, with this user mapped to root inside.
#: Named once and shared, because `user_namespaces_available` probes with these
#: and `conftest` re-executes with them — and a probe of flags other than the
#: ones about to be used is the bug that probe exists to prevent.
USERNS_FLAGS = ("-U", "-r", "-n", "-m")

#: Knobs that refuse a user namespace, and the value that means refused. Read
#: only to name a likely cause *after* an attempt has failed. The third is the
#: one that matters in practice: it is the Ubuntu 24.04 default, and the two
#: above it both read as permissive on exactly the machines it blocks.
USERNS_KNOBS = (
    ("/proc/sys/user/max_user_namespaces", 0),
    ("/proc/sys/kernel/unprivileged_userns_clone", 0),
    ("/proc/sys/kernel/apparmor_restrict_unprivileged_userns", 1),
)


class LabError(RuntimeError):
    """The network could not be built as asked.

    Deliberately not a skip. A lab that cannot construct its topology must fail
    loudly: quietly running a weaker network and passing is the one outcome
    worse than not running at all.
    """


@dataclass
class Ran:
    """What a command did."""

    argv: list[str]
    returncode: int
    stdout: str
    stderr: str

    @property
    def ok(self) -> bool:
        return self.returncode == 0


def run(
    argv: list[str],
    *,
    netns: str | None = None,
    check: bool = True,
    timeout: float = 30.0,
) -> Ran:
    """Runs a command, optionally inside a namespace."""
    if netns is not None:
        argv = ["ip", "netns", "exec", netns, *argv]

    completed = subprocess.run(
        argv,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    ran = Ran(argv, completed.returncode, completed.stdout, completed.stderr)

    if check and not ran.ok:
        raise LabError(
            f"{' '.join(argv)} exited {ran.returncode}\n"
            f"stdout: {ran.stdout.strip()}\n"
            f"stderr: {ran.stderr.strip()}"
        )

    return ran


def effective_capabilities() -> int:
    """This process's effective capability mask."""
    with open("/proc/self/status", encoding="ascii") as status:
        for line in status:
            if line.startswith("CapEff:"):
                return int(line.split()[1], 16)
    return 0


def has_net_admin() -> bool:
    return bool(effective_capabilities() & (1 << CAP_NET_ADMIN))


def missing_tools() -> list[str]:
    return [tool for tool in REQUIRED_TOOLS if shutil.which(tool) is None]


def user_namespaces_available() -> tuple[bool, str]:
    """Whether this process can get a user namespace — asked by getting one.

    By doing rather than by predicting, and the difference is not academic.
    This used to read two sysctls and infer the answer. On any Ubuntu 24.04 or
    later machine both of them say yes and the answer is no: AppArmor refuses
    the `uid_map` write through a third knob that neither sysctl mentions, and
    that restriction is the distribution default rather than a hardened
    exception.

    A wrong yes does not degrade to a skip. The caller acts on this by
    `execvp`-ing `unshare`, so there is no surviving process to report
    anything: the pytest session is replaced by one that prints
    `write failed /proc/self/uid_map: Operation not permitted` and exits 1, and
    a whole run disappears with no test report and no reason. That is the
    opposite of the clean skip this probe exists to produce.

    So the probe runs the same flags the re-execution will use, on a command
    that does nothing, and reports what it said. Proxies are read afterwards
    only to name a likely cause.
    """
    attempt = run(["unshare", *USERNS_FLAGS, "true"], check=False, timeout=15.0)
    if attempt.ok:
        return True, f"`unshare {' '.join(USERNS_FLAGS)}` works"

    # `unshare` prefixes its own complaints with its name, which the quoted
    # command has already supplied.
    said = attempt.stderr.strip().removeprefix("unshare: ")
    return False, (
        f"`unshare {' '.join(USERNS_FLAGS)}` "
        f"{said or f'exited {attempt.returncode} silently'}"
        f"{_refusing_knob()}"
    )


def _refusing_knob() -> str:
    """Names a knob that is set to refuse, if one is, for the skip message.

    Worth the few lines because `Operation not permitted` alone sends a reader
    hunting for a container runtime or a seccomp filter, when on a current
    Ubuntu the cause is one sysctl and so is the remedy. A hint, not a
    decision: the attempt above has already settled the answer.
    """
    for path, refusing in USERNS_KNOBS:
        try:
            with open(path, encoding="ascii") as knob:
                value = int(knob.read().strip())
        except (OSError, ValueError):
            continue

        if value == refusing:
            return f"; {path} is {value}"

    return ""


def mount_namespace_directory() -> None:
    """Gives `ip netns` somewhere writable to keep its namespaces.

    Inside a user namespace this process is root, but file ownership is not
    remapped: `/run` still belongs to real uid 0, which appears as `nobody`
    here, so creating a directory in it fails. A tmpfs mounted over the
    namespace directory is writable and — because the mount namespace is
    private — invisible to the rest of the machine.

    Shadowing only `/run/netns` is preferred to shadowing `/run`, which would
    hide whatever else a spawned process expects to find there.
    """
    if os.path.isdir(NETNS_DIR):
        run(["mount", "-t", "tmpfs", "tmpfs", NETNS_DIR])
        return

    run(["mount", "-t", "tmpfs", "tmpfs", "/run"])
    os.makedirs(NETNS_DIR, exist_ok=True)


@dataclass
class Link:
    """One veth pair, and the two addresses on its ends."""

    left: str
    right: str
    left_interface: str
    right_interface: str
    left_address: str
    right_address: str
    prefix: int


@dataclass
class Lab:
    """A set of namespaces and the links between them.

    Used as a context manager so a failed assertion tears the network down as
    reliably as a passing one. Namespaces outliving a run would make the next
    run's `ip netns add` fail, and the resulting error would point at the wrong
    test.
    """

    namespaces: list[str] = field(default_factory=list)
    links: list[Link] = field(default_factory=list)
    _pair_index: int = 0

    def __enter__(self) -> "Lab":
        return self

    def __exit__(self, *_exception: object) -> None:
        self.teardown()

    def teardown(self) -> None:
        for name in reversed(self.namespaces):
            # Deleting a namespace takes its interfaces with it, so the veth
            # pairs need no separate cleanup. `check=False` because teardown
            # runs on the failure path too and must not mask the real error.
            run(["ip", "netns", "delete", name], check=False)
        self.namespaces.clear()
        self.links.clear()

    # -- construction ----------------------------------------------------

    def add(self, name: str) -> str:
        """Creates a namespace with loopback up.

        Loopback is not up by default in a fresh namespace, and a process that
        binds `127.0.0.1` fails in a way that looks like the process is broken
        rather than the network.
        """
        run(["ip", "netns", "add", name])
        self.namespaces.append(name)
        run(["ip", "link", "set", "lo", "up"], netns=name)
        return name

    def connect(self, left: str, right: str, subnet: str, prefix: int = 24) -> Link:
        """Joins two namespaces with a veth pair on `subnet`.

        `left` takes `.1` and `right` takes `.2`. The pair is created with
        temporary names and renamed after the move, because both ends exist in
        this process's namespace for the moment between creation and the move —
        and two links into the same router would both want to be called
        `to-router` there.
        """
        if not re.fullmatch(r"\d+\.\d+\.\d+\.0", subnet):
            raise LabError(f"expected a /24 network address like 10.10.0.0, got {subnet}")

        base = subnet.rsplit(".", 1)[0]
        self._pair_index += 1
        staging_left = f"veth{self._pair_index}a"
        staging_right = f"veth{self._pair_index}b"

        link = Link(
            left=left,
            right=right,
            left_interface=_interface_name(right),
            right_interface=_interface_name(left),
            left_address=f"{base}.1",
            right_address=f"{base}.2",
            prefix=prefix,
        )

        run(["ip", "link", "add", staging_left, "type", "veth", "peer", "name", staging_right])
        run(["ip", "link", "set", staging_left, "netns", left])
        run(["ip", "link", "set", staging_right, "netns", right])

        for namespace, staged, interface, address in (
            (left, staging_left, link.left_interface, link.left_address),
            (right, staging_right, link.right_interface, link.right_address),
        ):
            run(["ip", "link", "set", staged, "name", interface], netns=namespace)
            run(
                ["ip", "addr", "add", f"{address}/{prefix}", "dev", interface],
                netns=namespace,
            )
            run(["ip", "link", "set", interface, "up"], netns=namespace)

        self.links.append(link)
        return link

    def route_default(self, namespace: str, via: str) -> None:
        run(["ip", "route", "add", "default", "via", via], netns=namespace)

    def route(self, namespace: str, subnet: str, via: str, prefix: int = 24) -> None:
        """Teaches one namespace how to reach a network behind another.

        Used to make a segment reachable *without* translation, which is the
        difference between a routed LAN and a NAT. Adding one of these to a
        topology that is meant to be translated would be a hole straight through
        the NAT under test: a peer advertising its private address would become
        reachable at it, the punch would succeed by bypassing the translation
        entirely, and the topology would pass while testing a plain LAN.
        """
        run(
            ["ip", "route", "add", f"{subnet}/{prefix}", "via", via],
            netns=namespace,
        )

    def forward(self, namespace: str) -> None:
        """Makes a namespace a router."""
        run(["sysctl", "-w", "net.ipv4.ip_forward=1"], netns=namespace)

    # -- impairments -----------------------------------------------------

    def masquerade(self, namespace: str, interface: str, *, symmetric: bool = False) -> None:
        """Source-NATs everything leaving `interface`.

        This is `iptables` plus `nf_conntrack`, which models a home router and
        is not a carrier-grade NAT. See `README.md`.

        `symmetric` is the difference between a NAT a hole can be punched
        through and one it cannot, and it is one flag:

        - **Without it**, netfilter tries to preserve the source port, so every
          destination sees the same external port for a given internal socket.
          That is an endpoint-independent mapping, and it is what makes hole
          punching possible at all: the mapping a peer learned through the relay
          is the mapping it can then send to.
        - **With `--random-fully`**, a fresh external port is drawn for each
          conntrack entry, and a different destination is a different entry. The
          mapping the relay observed is therefore not the mapping the peer can
          use, which is the defining property of a symmetric NAT.

        Neither claim is taken on trust. `observed_source_ports` measures which
        behaviour a built topology actually has, because the plan's risk list is
        right that getting this wrong produces a full-cone test wearing a
        symmetric label — and it would pass, since the fallback fires whenever
        the direct path fails for any reason at all.
        """
        rule = [
            "iptables", "-t", "nat", "-A", "POSTROUTING",
            "-o", interface, "-j", "MASQUERADE",
        ]
        if symmetric:
            rule.append("--random-fully")

        run(rule, netns=namespace)

    def drop_udp(self, namespace: str) -> None:
        """Drops forwarded UDP, which is what a UDP-hostile network looks like.

        `FORWARD` rather than `INPUT`/`OUTPUT`, so it is the router refusing to
        carry UDP rather than the endpoints refusing to speak it. The endpoints
        must still be able to open a socket and try, because trying and failing
        is the behaviour under test.
        """
        run(["iptables", "-A", "FORWARD", "-p", "udp", "-j", "DROP"], netns=namespace)

    def delay(self, namespace: str, interface: str, milliseconds: float) -> None:
        """Adds one-way delay on an interface.

        Applied per interface and per direction, so a round trip crossing two
        interfaces sees twice what is asked for here. `topologies.py` does that
        arithmetic once rather than leaving it to each caller.
        """
        self._netem(namespace, interface, ["delay", f"{milliseconds}ms"])

    def loss(self, namespace: str, interface: str, percent: float) -> None:
        """Adds independent uniform loss. Real loss is bursty; this is not."""
        self._netem(namespace, interface, ["loss", f"{percent}%"])

    def _netem(self, namespace: str, interface: str, arguments: list[str]) -> None:
        existing = run(
            ["tc", "qdisc", "show", "dev", interface],
            netns=namespace,
        ).stdout

        verb = "change" if "netem" in existing else "add"
        run(
            ["tc", "qdisc", verb, "dev", interface, "root", "netem", *arguments],
            netns=namespace,
        )

    # -- inspection ------------------------------------------------------

    def measure_rtt(self, namespace: str, target: str, count: int = 5) -> float:
        """Round-trip time in milliseconds, measured rather than assumed.

        A topology that did not come out as asked must fail the run rather than
        producing a number that looks like a finding. This is what lets the
        throughput lane say the RTT it divides by is the RTT that existed.

        The first packet to an unresolved neighbour is discarded, because it
        does not measure the link. Resolving the address costs its own round
        trip across the same delayed path, so the first reply arrives at twice
        the RTT and drags the average up by `RTT / count` — on a 50 ms link,
        `min/avg/max` reads `100.3/120.4/200.9` cold and `100.05/100.07/100.09`
        once warm. Averaging the cold run would have this lane divide by an RTT
        20% larger than the one the transfer actually saw, and the error grows
        with the delay being injected.
        """
        run(
            ["ping", "-c", "1", "-W", "5", target],
            netns=namespace,
            check=False,
            timeout=30.0,
        )

        ran = run(
            ["ping", "-c", str(count), "-i", "0.2", "-W", "5", target],
            netns=namespace,
            timeout=60.0,
        )

        match = re.search(r"= [\d.]+/([\d.]+)/", ran.stdout)
        if match is None:
            raise LabError(f"could not read a round-trip time from:\n{ran.stdout}")

        return float(match.group(1))

    def netem_on(self, namespace: str, interface: str) -> str:
        """What `tc` believes it is doing to an interface, as it reports it.

        Read back rather than remembered. An impairment this lab asked for and
        `tc` silently declined would leave every topology below it describing a
        network that was never built, and the tests would pass.
        """
        for line in run(
            ["tc", "qdisc", "show", "dev", interface], netns=namespace
        ).stdout.splitlines():
            if "netem" in line:
                return line.strip()
        return ""

    def measure_loss(self, namespace: str, target: str, count: int = 200) -> float:
        """Percentage of probes that did not come back.

        Evidence for a report rather than a gate. The outcome is binomial, so a
        run that happens to lose nothing is an unremarkable event at these rates
        and must not be able to fail a build; what is asserted instead is that
        the qdisc really carries the setting.
        """
        ran = run(
            ["ping", "-c", str(count), "-i", "0.01", "-W", "2", target],
            netns=namespace,
            check=False,
            timeout=120.0,
        )

        match = re.search(r"([\d.]+)% packet loss", ran.stdout)
        if match is None:
            raise LabError(f"could not read a loss figure from:\n{ran.stdout}")

        return float(match.group(1))

    def reaches(self, namespace: str, target: str) -> bool:
        return run(
            ["ping", "-c", "1", "-W", "3", target],
            netns=namespace,
            check=False,
            timeout=30.0,
        ).ok

    def interface_bytes(self, namespace: str, interface: str) -> int:
        """Bytes an interface has carried, both directions summed.

        This is how the lab tells a hole punch from a relayed connection, and it
        is the only way available to it: `drop --status` reports `path=p2p`
        whenever no *Drop* server was involved, which is true whether iroh
        punched through the NAT or quietly carried the same QUIC connection over
        its relay. `cli/src/direct.rs` says so in as many words — "a failed hole
        punch is not a failed connection".

        So the question "was a hole punched" is not one the transfer can be
        asked; it is one the *network* answers. A relay link that carried
        kilobytes saw signalling, and one that carried the whole payload was the
        path. Counting bytes needs no cooperation from either peer and cannot be
        satisfied by a status string that means something adjacent.

        Both directions are summed because which one carries the payload depends
        on which end is which, and the question here is only whether the
        payload went past at all.
        """
        ran = run(["ip", "-s", "-j", "link", "show", "dev", interface], netns=namespace)

        try:
            stats = json.loads(ran.stdout)[0]["stats64"]
        except (json.JSONDecodeError, KeyError, IndexError) as error:
            raise LabError(
                f"could not read counters for {interface} in {namespace}: {error}\n{ran.stdout}"
            ) from error

        return int(stats["rx"]["bytes"]) + int(stats["tx"]["bytes"])

    def observed_source_ports(
        self,
        sender: str,
        observer: str,
        target: str,
        ports: tuple[int, int],
    ) -> tuple[int, int]:
        """What two destinations saw as the source port of one socket.

        The direct measurement of NAT mapping behaviour, and the plan's risk
        list asks for exactly this before either NAT topology is trusted. The
        distinction that decides whether hole punching can work is whether a
        NAT's external port depends on **where the packet is going**:

        ```text
            same port to both destinations   endpoint-independent — punchable
            different port to each           endpoint-dependent — symmetric
        ```

        One socket, one source port, two destinations, and the observer reports
        what arrived. Nothing here infers the behaviour from the `iptables`
        rules that were installed, which is the mistake the plan warns about:
        get the rules wrong and a symmetric topology is a full-cone test that
        passes, because the fallback fires whenever the direct path fails *for
        any reason*.

        Both probes leave from the same bound socket, so any difference is the
        NAT's and not the sender's.
        """
        observer_process = subprocess.Popen(
            ["ip", "netns", "exec", observer, sys.executable, "-c", _OBSERVER, ",".join(
                str(port) for port in ports
            )],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )

        try:
            if (observer_process.stdout.readline().strip() or None) != "ready":
                _, stderr = observer_process.communicate(timeout=10)
                raise LabError(f"the port observer never bound its sockets:\n{stderr}")

            run(
                [
                    sys.executable, "-c", _PROBE, target,
                    ",".join(str(port) for port in ports),
                ],
                netns=sender,
                timeout=30.0,
            )

            stdout, stderr = observer_process.communicate(timeout=30)
        finally:
            if observer_process.poll() is None:
                os.killpg(os.getpgid(observer_process.pid), signal.SIGKILL)
                observer_process.wait(timeout=10)

        seen = stdout.strip().splitlines()[-1] if stdout.strip() else ""
        try:
            observed = json.loads(seen)
        except json.JSONDecodeError as error:
            raise LabError(
                f"the port observer reported nothing usable: {error}\n"
                f"stdout: {stdout}\nstderr: {stderr}"
            ) from error

        missing = [port for port in ports if str(port) not in observed]
        if missing:
            raise LabError(
                f"no probe reached {missing} — the sender could not traverse to {target}, "
                f"so nothing can be said about the mapping. Observed: {observed}"
            )

        return tuple(int(observed[str(port)]) for port in ports)


#: Binds one UDP socket per port, says `ready`, then reports the source port
#: each one saw as JSON. Inline rather than a file in the repository because it
#: is four lines of socket handling with no Drop in it, and a separate script
#: would be a file nobody finds when reading the method that runs it.
_OBSERVER = """
import json, select, socket, sys
ports = [int(p) for p in sys.argv[1].split(',')]
socks = {}
for port in ports:
    handle = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    handle.bind(('0.0.0.0', port))
    socks[handle] = port
print('ready', flush=True)
seen = {}
# Selected rather than read in order: the probes are independent datagrams and
# may arrive in either order, and a blocking read on the wrong one would wait
# out the deadline with the answer already sitting in the other socket.
deadline = 15.0
while len(seen) < len(ports) and deadline > 0:
    ready, _, _ = select.select(list(socks), [], [], 1.0)
    if not ready:
        deadline -= 1.0
        continue
    for handle in ready:
        _, peer = handle.recvfrom(64)
        seen[socks[handle]] = peer[1]
print(json.dumps(seen), flush=True)
"""

#: Sends one datagram to each port from a single socket, so the source port is
#: identical on both and any difference the observer sees belongs to the NAT.
_PROBE = """
import socket, sys, time
target = sys.argv[1]
handle = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
handle.bind(('0.0.0.0', 0))
for port in (int(p) for p in sys.argv[2].split(',')):
    handle.sendto(b'probe', (target, port))
    # A beat between them, so the second is a new conntrack entry rather than a
    # retransmission the kernel might coalesce onto the first.
    time.sleep(0.2)
"""


def _interface_name(peer: str) -> str:
    """`to-router`, truncated to what the kernel accepts."""
    return f"to-{peer}"[:15]
