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

    @property
    def ok(self) -> bool:
        return self.sender.ok and self.receiver.ok

    @property
    def mib_per_second(self) -> float:
        if not self.ok or self.seconds <= 0:
            raise LabError("a transfer that did not complete has no throughput")
        return self.bytes_sent / self.seconds / (1024 * 1024)

    def why_it_failed(self) -> str:
        return (
            f"sender exited {self.sender.returncode}:\n{self.sender.stderr}\n"
            f"receiver exited {self.receiver.returncode}:\n{self.receiver.stderr}"
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
            sender_out, sender_err = _wait(sender, timeout)
            return Transfer(
                sender=Half(sender.returncode, sender_out, sender_err),
                receiver=Half(-1, "", "the receiver was never started"),
                seconds=0.0,
                bytes_sent=payload_size,
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

        receiver_out, receiver_err = _wait(receiver, timeout)
        sender_out, sender_err = _wait(sender, timeout)
        seconds = time.monotonic() - started
    finally:
        _terminate(sender)

    return Transfer(
        sender=Half(sender.returncode, sender_out, sender_err),
        receiver=Half(receiver.returncode, receiver_out, receiver_err),
        seconds=seconds,
        bytes_sent=payload_size,
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


def _wait(process: subprocess.Popen[str], timeout: float) -> tuple[str, str]:
    try:
        return process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        _terminate(process)
        raise LabError(f"a transfer did not finish within {timeout:.0f}s") from None


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
