"""Fixtures, and the one piece of machinery that makes the rest possible.

# Getting CAP_NET_ADMIN without being given it

Building a topology needs `CAP_NET_ADMIN`, and the obvious way to get it is to
run the lab as root. That is a poor thing to ask of anyone who wants to run the
tests, so the lab does not ask: an unprivileged user namespace grants full
capabilities *inside itself*, and a network namespace created within one
accepts every `ip`, `iptables` and `tc` command this lab issues.

So there are three cases, and the difference between them matters enough to be
reported rather than collapsed into one skip:

1. **The capability is already held** — running as root, or with it granted.
   Use it.
2. **A user namespace can be obtained.** Re-execute the whole pytest session
   inside `unshare -Urnm` and carry on. This is the ordinary case and needs no
   privilege at all.
3. **Neither.** Some kernels disable unprivileged user namespaces, and some
   container runtimes block the syscall. Skip, saying which of the two was
   tried and what it said.

The re-execution happens in `pytest_configure`, before collection, and guards
itself with an environment variable so a failure to gain the capability cannot
become a fork bomb. It has to suspend pytest's global capture first: by that
point pytest has replaced file descriptors 1 and 2 with its own buffers, and a
process that replaces itself inherits them — so the new session would write its
entire output into a buffer that nobody is left alive to read, and the run would
appear to succeed silently.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))

import netns  # noqa: E402
import runner  # noqa: E402

#: Set on the re-executed process so it does not try again. Its presence means
#: "an attempt was already made", not "the attempt succeeded".
REENTRY = "NETLAB_NAMESPACED"

#: Why the lab cannot run, or `None`. Decided in `pytest_configure`.
_unavailable: str | None = None


def _prepare(config: pytest.Config) -> str | None:
    missing = netns.missing_tools()
    if missing:
        return f"missing required tools: {', '.join(missing)}"

    if netns.has_net_admin():
        return None

    if os.environ.get(REENTRY):
        # The re-execution happened and still produced no capability. Something
        # took it away that this module cannot see — a seccomp filter, or an
        # LSM — so report the fact rather than guessing at the cause.
        return (
            "re-executed inside `unshare -Urnm` but still hold no CAP_NET_ADMIN; "
            "something outside this process is refusing it"
        )

    willing, why = netns.user_namespaces_available()
    if not willing:
        return f"no CAP_NET_ADMIN and no unprivileged user namespaces ({why})"

    os.environ[REENTRY] = "1"
    argv = ["unshare", "-Urnm", sys.executable, "-m", "pytest", *sys.argv[1:]]

    # Hand the real terminal back before the process is replaced. Without this
    # the re-executed session writes into pytest's capture buffers, which die
    # with the process that owned them — an entire run, silently discarded.
    capture = config.pluginmanager.get_plugin("capturemanager")
    if capture is not None:
        capture.suspend_global_capture(in_=True)

    try:
        os.execvp("unshare", argv)
    except OSError as error:  # pragma: no cover - depends on the kernel
        return f"could not re-execute inside a user namespace: {error}"

    raise AssertionError("unreachable: execvp replaces this process")


def pytest_configure(config: pytest.Config) -> None:
    global _unavailable

    _unavailable = _prepare(config)
    if _unavailable is None:
        netns.mount_namespace_directory()


def pytest_collection_modifyitems(items: list[pytest.Item]) -> None:
    """Skips everything, once, with the reason — rather than per assertion.

    A lab that cannot build a network has nothing to say about Drop, so no test
    here is meaningful without one. Skipping at collection keeps the reason in
    one place and stops each test from having to guard itself.
    """
    if _unavailable is None:
        return

    skip = pytest.mark.skip(reason=f"the network lab needs a namespace: {_unavailable}")
    for item in items:
        item.add_marker(skip)


@pytest.fixture(scope="session")
def binaries() -> runner.Binaries:
    """The real `drop` and `api`, built once for the session, optimised.

    Release rather than debug, and that is a throughput decision rather than an
    impatient one. An unoptimised build moves about **6 MiB/s** through this
    lab against roughly **600 MiB/s** optimised, because the chunk sealing is
    doing AES-GCM with the bounds checks left in. At 6 MiB/s every ceiling this
    lab reasons about sits far above what the pipeline can reach, so the
    acknowledgement window could never be the binding constraint and the
    latency lane would measure the optimiser's absence and call it a protocol
    property.

    One profile for the whole session, not release for the lane that needs it:
    two builds would double the wait, and a failure would raise the question of
    which binary saw it.
    """
    return runner.build("release")


@pytest.fixture
def lab():
    """A fresh, empty network, torn down however the test ends.

    Function-scoped and using fixed namespace names, so these tests are serial
    by construction. Running two topologies at once would have them fight over
    the names, and a lab that raced itself would produce failures that look
    like Drop's.
    """
    with netns.Lab() as built:
        yield built


@pytest.fixture
def workspace(tmp_path: Path) -> Path:
    """Somewhere to put a synthetic payload and whatever arrives."""
    return tmp_path


#: What the run measured, in the order it was measured. Printed at the end of
#: every run by the hook below.
_measurements: list[tuple[str, str]] = []


@pytest.fixture
def record_measurement():
    """Records a number the run produced, for printing when it ends.

    A pass is not the whole result of a lane that measures something. A
    throughput ratio can come out right for the wrong reason — a link that is
    slow for an unrelated cause satisfies a ceiling just as well as an enforced
    window does — and that is visible in the numbers and invisible in a green
    tick. So the numbers are shown whether or not anything failed.

    Deliberately not pytest's `record_property`, which warns under the default
    `xunit2` family and records into an XML file nobody reading the terminal
    will open.
    """

    def record(label: str, value: object) -> None:
        _measurements.append((label, str(value)))

    return record


def pytest_terminal_summary(terminalreporter: pytest.TerminalReporter) -> None:
    if not _measurements:
        return

    terminalreporter.write_sep("=", "measured")
    width = max(len(label) for label, _ in _measurements)
    for label, value in _measurements:
        terminalreporter.write_line(f"{label:<{width}}  {value}")
