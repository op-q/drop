"""Builds the real binaries, runs them in namespaces, and reads what came out.

This module starts processes and inspects their output. It does not implement
any part of the Drop protocol — no envelope, no handshake, no framing, no chunk
sealing. `docs/decisions.md` entry 11 refuses a second implementation of the
envelope for the browser, and a Python one here would reintroduce exactly the
drift that decision prevents. What the lab knows about a transfer is what the
binaries said and what landed on disk.

# Why the status line and not the prose

`drop --status` prints one machine-readable line naming the carrier that moved
the bytes. The prose above it says the same thing to a person and is expected
to be reworded; matching on it would make this lab break on a copy edit.
"""

from __future__ import annotations

import hashlib
import os
import re
import shutil
import signal
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

from netns import LabError, run
from topologies import RELAY_ADDRESS, RELAY_PORT, Net

#: The line `drop --status` prints. Anchored at both ends: a partial match
#: would accept a longer line that meant something else.
STATUS = re.compile(r"^drop-status: path=(?P<path>\S+) fallback=(?P<fallback>\S+)$")

REPOSITORY = Path(__file__).resolve().parent.parent


@dataclass(frozen=True)
class Binaries:
    drop: Path
    api: Path


def build(profile: str = "debug") -> Binaries:
    """Builds the workspace binaries and returns where they landed.

    Built once per pytest session. `--bins` rather than a plain build because
    the lab needs executables and nothing else, and the workspace's test
    targets take considerably longer than its binaries.
    """
    argv = ["cargo", "build", "--workspace", "--bins"]
    if profile == "release":
        argv.append("--release")

    completed = subprocess.run(
        argv,
        cwd=REPOSITORY,
        capture_output=True,
        text=True,
        check=False,
        timeout=900,
    )
    if completed.returncode != 0:
        raise LabError(f"cargo build failed:\n{completed.stderr}")

    binaries = Binaries(
        drop=REPOSITORY / "target" / profile / "drop",
        api=REPOSITORY / "target" / profile / "api",
    )
    for path in (binaries.drop, binaries.api):
        if not path.is_file():
            raise LabError(f"cargo build reported success but {path} is missing")

    return binaries


class Relay:
    """The `api` binary, running in its own namespace.

    A context manager because a relay outliving its test would hold the port
    and make the next test fail somewhere unrelated. Nothing about its
    configuration is tuned for the lab: the bounds in `src/config.rs` are
    compile-time on purpose, and a topology that wanted a different limit does
    not get one.
    """

    def __init__(self, binaries: Binaries, net: Net) -> None:
        if net.relay is None:
            raise LabError("this topology has no relay namespace to run one in")

        self._binaries = binaries
        self._net = net
        self._process: subprocess.Popen[str] | None = None

    def __enter__(self) -> "Relay":
        environment = {
            **os.environ,
            "DROP_BIND_ADDR": f"{RELAY_ADDRESS}:{RELAY_PORT}",
            "RUST_LOG": "warn",
        }

        self._process = subprocess.Popen(
            ["ip", "netns", "exec", self._net.relay, str(self._binaries.api)],
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        self._await_health()
        return self

    def __exit__(self, *_exception: object) -> None:
        if self._process is None:
            return

        # The whole group: `ip netns exec` is the child and `api` is its child,
        # so signalling only the former leaves the relay holding the port.
        try:
            os.killpg(os.getpgid(self._process.pid), signal.SIGTERM)
        except ProcessLookupError:
            pass

        try:
            self._process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            os.killpg(os.getpgid(self._process.pid), signal.SIGKILL)
            self._process.wait(timeout=15)

    def _await_health(self, seconds: float = 30.0) -> None:
        """Waits for `/health`, from a namespace that has to route to reach it.

        Checked from the sender rather than from the relay's own namespace: a
        relay that answers only itself is not reachable by the peers, and the
        difference is exactly what a routing mistake looks like.
        """
        deadline = time.monotonic() + seconds

        while time.monotonic() < deadline:
            probe = run(
                ["curl", "-fsS", "-m", "2", f"http://{RELAY_ADDRESS}:{RELAY_PORT}/health"],
                netns=self._net.sender,
                check=False,
                timeout=10.0,
            )
            if probe.ok:
                return

            if self._process is not None and self._process.poll() is not None:
                _, stderr = self._process.communicate()
                raise LabError(f"the relay exited before answering:\n{stderr}")

            time.sleep(0.25)

        raise LabError(f"the relay did not answer /health within {seconds:.0f}s")


@dataclass
class Half:
    """One side of a transfer, after it finished."""

    returncode: int
    stdout: str
    stderr: str

    @property
    def ok(self) -> bool:
        return self.returncode == 0

    @property
    def carrier(self) -> str | None:
        """`p2p` or `relay`, from the status line, or `None` if it said none."""
        return self._status("path")

    @property
    def fallback(self) -> str | None:
        return self._status("fallback")

    def _status(self, field: str) -> str | None:
        for line in self.stderr.splitlines():
            match = STATUS.match(line.strip())
            if match is not None:
                return match.group(field)
        return None


@dataclass
class Transfer:
    """A whole transfer, from both ends."""

    sender: Half
    receiver: Half
    seconds: float
    bytes_sent: int
    #: An end was still running when the deadline passed and had to be killed.
    #: Reported rather than raised: a transfer that hangs is a *result*, and one
    #: of the more interesting ones this lab can produce — a flow-control or
    #: acknowledgement bug looks exactly like this from outside. Raising would
    #: file it as a broken lab, which is what `LabError` is for.
    timed_out: bool = False

    @property
    def ok(self) -> bool:
        return not self.timed_out and self.sender.ok and self.receiver.ok

    @property
    def mib_per_second(self) -> float:
        if not self.ok or self.seconds <= 0:
            raise LabError("a transfer that did not complete has no throughput")
        return self.bytes_sent / self.seconds / (1024 * 1024)

    def why_it_failed(self) -> str:
        timed_out = " (killed at the deadline)" if self.timed_out else ""
        return (
            f"sender exited {self.sender.returncode}{timed_out}:\n{self.sender.stderr}\n"
            f"receiver exited {self.receiver.returncode}{timed_out}:\n{self.receiver.stderr}"
        )


def transfer(
    binaries: Binaries,
    net: Net,
    source: Path,
    destination: Path,
    *,
    transport: str = "auto",
    timeout: float = 240.0,
) -> Transfer:
    """Sends `source` from the sender namespace to the receiver namespace.

    The code is read from the sender's stdout, which is where `drop send` puts
    it and nothing else. Timing starts when the receiver is launched rather
    than when the sender is: the sender spends its first seconds looking for a
    peer-to-peer path, and counting that as transfer time would understate
    every throughput figure by an amount that varies with the topology.
    """
    destination.mkdir(parents=True, exist_ok=True)
    payload_size = source.stat().st_size

    environment = {**os.environ, "DROP_STATUS": "1"}
    common = ["--transport", transport]
    if net.relay_url is not None:
        common += ["--server", net.relay_url]

    sender = subprocess.Popen(
        [
            "ip", "netns", "exec", net.sender, str(binaries.drop),
            "send", str(source), *common,
        ],
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )

    try:
        code = _read_code(sender)

        if code is None:
            # A sender that dies before announcing a code is a transfer that
            # failed, not a lab that broke — the relay being unreachable is a
            # result worth asserting on. Reported as an outcome so a caller can
            # tell the two apart, which is the whole reason `LabError` exists.
            sender_out, sender_err, sender_hung = _wait(sender, timeout)
            return Transfer(
                sender=Half(sender.returncode, sender_out, sender_err),
                receiver=Half(-1, "", "the receiver was never started"),
                seconds=0.0,
                bytes_sent=payload_size,
                timed_out=sender_hung,
            )

        started = time.monotonic()
        receiver = subprocess.Popen(
            [
                "ip", "netns", "exec", net.receiver, str(binaries.drop),
                "recv", code, *common, "--out", str(destination),
            ],
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )

        receiver_out, receiver_err, receiver_hung = _wait(receiver, timeout)
        sender_out, sender_err, sender_hung = _wait(sender, timeout)
        seconds = time.monotonic() - started
    finally:
        _terminate(sender)

    return Transfer(
        sender=Half(sender.returncode, sender_out, sender_err),
        receiver=Half(receiver.returncode, receiver_out, receiver_err),
        seconds=seconds,
        bytes_sent=payload_size,
        timed_out=sender_hung or receiver_hung,
    )


def _read_code(sender: subprocess.Popen[str]) -> str | None:
    """Reads the one line `drop send` writes to stdout, or `None` if it died.

    Blocking on `readline` is safe here and a timeout is not available on it,
    so the deadline is the caller's overall timeout and the sender exiting — a
    sender that failed closes stdout, and the read returns empty rather than
    hanging.
    """
    assert sender.stdout is not None
    return sender.stdout.readline().strip() or None


def _wait(process: subprocess.Popen[str], timeout: float) -> tuple[str, str, bool]:
    """Collects a process's output, reporting a deadline rather than raising."""
    try:
        stdout, stderr = process.communicate(timeout=timeout)
        return stdout, stderr, False
    except subprocess.TimeoutExpired:
        _terminate(process)
        try:
            # The group is gone, so the pipes are closed and this returns what
            # was written before the kill — which is where a hung transfer says
            # how far it got.
            stdout, stderr = process.communicate(timeout=10)
        except subprocess.TimeoutExpired:  # pragma: no cover - the pipes outlived SIGKILL
            stdout, stderr = "", ""
        return stdout, stderr, True


def _terminate(process: subprocess.Popen[str]) -> None:
    if process.poll() is not None:
        return

    try:
        os.killpg(os.getpgid(process.pid), signal.SIGKILL)
    except ProcessLookupError:
        return

    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass


def synthetic_payload(path: Path, size: int, *, seed: int = 0) -> bytes:
    """Writes `size` bytes of reproducible pseudo-random data.

    Incompressible on purpose, so a topology measuring throughput measures the
    network rather than gzip, and so `--compress` cannot quietly change what is
    being timed. Generated rather than taken from anywhere: `AGENTS.md` requires
    synthetic fixtures, and a real file has no place in this repository.
    """
    digest = hashlib.blake2b(seed.to_bytes(8, "big"), digest_size=64)
    blocks = []
    produced = 0

    while produced < size:
        digest = hashlib.blake2b(digest.digest(), digest_size=64)
        block = digest.digest()
        blocks.append(block)
        produced += len(block)

    contents = b"".join(blocks)[:size]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(contents)
    return contents


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def relay_is_running() -> bool:
    """Whether any `api` process exists, for topologies that claim none does.

    Asserted rather than assumed: "no Drop server was in the path" is the
    strongest claim this lab can make, and it should rest on a check rather
    than on the test having not started one.
    """
    if shutil.which("pgrep") is None:
        raise LabError("pgrep is needed to prove no relay is running")

    return run(["pgrep", "-x", "api"], check=False).ok


@dataclass
class Rate:
    """A streaming rate, with the cost of setting the transfer up removed."""

    mib_per_second: float
    #: The intercept: what a transfer costs before any payload moves. Reported
    #: because it is large on a slow link and explains the raw figures.
    setup_seconds: float
    #: `(MiB, seconds)` for each transfer the rate was derived from.
    points: list[tuple[float, float]]

    def __str__(self) -> str:
        measured = ", ".join(f"{mib:.0f}MiB in {s:.2f}s" for mib, s in self.points)
        return (
            f"{self.mib_per_second:.1f} MiB/s streaming "
            f"(setup {self.setup_seconds:.2f}s; {measured})"
        )


def measure_streaming_rate(
    binaries: Binaries,
    net: Net,
    workspace: Path,
    small: int,
    large: int,
    *,
    transport: str = "relay",
) -> Rate:
    """How fast bytes actually stream, timed by difference rather than directly.

    A transfer's wall clock is `setup + bytes / rate`, and on a high-latency
    link the setup term is not a rounding error: it is a handshake whose cost
    is several round trips, so it *grows with the very quantity this lane
    varies*. Timing one transfer and dividing would therefore measure the
    handshake and report it as throughput — and would do so in a way that still
    looked inversely proportional to the RTT, which is the shape this lane
    exists to check. That is the plan's "passes while proving less than it
    looks like", and it would have been invisible.

    Two payloads and a slope remove the term instead of estimating it:

    ```text
        rate = (large - small) / (seconds(large) - seconds(small))
    ```

    Whatever setup costs, it costs the same in both runs and cancels. Making
    the payload large enough to drown it is not an alternative here: at an
    800 ms loop the setup is around 10 s, so a run would need most of a
    gigabyte before the error fell under a tenth.
    """
    if large <= small:
        raise LabError(f"the large payload must exceed the small one, got {large} and {small}")

    points = []
    for size in (small, large):
        source = workspace / f"payload-{size}.bin"
        destination = workspace / f"received-{size}"
        synthetic_payload(source, size)

        with Relay(binaries, net):
            result = transfer(binaries, net, source, destination, transport=transport)

        if not result.ok:
            raise LabError(f"a rate measurement needs a completed transfer:\n{result.why_it_failed()}")

        arrived = destination / source.name
        if sha256(arrived) != sha256(source):
            raise LabError(f"{size} bytes arrived corrupted, so its timing means nothing")

        points.append((size / 1024 / 1024, result.seconds))

    (small_mib, small_seconds), (large_mib, large_seconds) = points
    if large_seconds <= small_seconds:
        raise LabError(
            f"the larger payload was not slower ({large_seconds:.2f}s against "
            f"{small_seconds:.2f}s), so this link is too fast to time by difference"
        )

    rate = (large_mib - small_mib) / (large_seconds - small_seconds)
    return Rate(
        mib_per_second=rate,
        setup_seconds=small_seconds - small_mib / rate,
        points=points,
    )
