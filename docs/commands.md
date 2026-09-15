# Commands

Short commands for humans and agents working on Drop. The authoritative
validation set lives in [AGENTS.md](../AGENTS.md); this file is the practical
index, including the things that are easy to forget.

## Full check (run before opening a pull request)

```bash
scripts/check-secrets.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

The repository is a Cargo workspace with three members — `api`, `drop-cli`,
and `drop-crypto`. A root-package-only run misses all but the relay, which is
why every Rust command here is workspace-wide.

CI runs Clippy and the tests on Linux, macOS and Windows. Passing locally on one
of them says nothing about the other two.

## Secret scan

```bash
scripts/check-secrets.sh
```

Required before any publication. Also run `git diff --check`.

## Rust tests

```bash
cargo test --workspace --all-targets          # everything
cargo test -p api                             # relay only
cargo test -p drop-cli                        # CLI only
cargo test -p drop-cli --test transfer        # CLI transfer tests
```

## Run the relay locally

```bash
cargo run
```

Then `/health`, `/ready`, and `/metrics` on `http://127.0.0.1:8080`. The root
answers 404 with a line saying what the host is; there is no web page.

Bind elsewhere with `DROP_BIND_ADDR=127.0.0.1:8080 cargo run`.

## Local transfer

Nothing here needs a published release. The relay and the `drop` binary both
come out of this checkout, and the whole round trip stays on loopback.

The scripted version, which starts a relay, sends, receives and compares the
bytes:

```bash
scripts/dev-transfer.sh                 # a generated 2.9 MiB file, over one chunk
scripts/dev-transfer.sh ./some-folder   # something of your own
```

It exits non-zero if the received bytes differ, so it is usable as a smoke test.
`DROP_DEV_PORT` moves it off 8080; an existing listener on that port is reused
rather than fought over.

By hand, which is what you want when poking at a failure. With a relay running,
in two terminals:

```bash
cargo run -p drop-cli -- send ./some-folder --server http://127.0.0.1:8080
cargo run -p drop-cli -- recv <CODE> --server http://127.0.0.1:8080 --out /tmp/drop-test
```

`scripts/dev-transfer.sh --relay` starts just the relay and prints the two
commands with the port filled in.

`--server` and `DROP_SERVER` are equivalent, and there is no default: without
either, the CLI has no relay and stays on the direct path. `--transport relay`
without one is an error rather than a connection attempt. See
[`decisions.md`](decisions.md) entry 16.

**`--status` when something else is reading the output.** It adds one line
naming the carrier that actually moved the bytes, beside the prose that says
the same thing to a person:

```text
Path    relay (encrypted; the relay cannot read it)
drop-status: path=relay fallback=none
```

`path` is `p2p` or `relay`; `fallback` is `none`, `rendezvous` (a direct path
could not be set up) or `no-record` (the receiver looked and the sender was not
there, so the sender had already fallen back). `DROP_STATUS` turns it on for
every `drop` a harness spawns. Match on this rather than on the sentences,
which are written to be reworded.

**A mistyped code is worth seeing at least once.** Keep the nameplate, change a
word, and the receiver fails where it should — at the metadata, not the
handshake:

```text
$ drop recv A1B2C3-zone-zoo-zebra --server http://127.0.0.1:8080
error: could not decrypt the transfer details — check the code and try again
```

The sender's session is consumed by that attempt, and it exits saying the
receiver could not open the transfer. Over the relay that is the relay refusing
a second claim; on the direct path the sender counts the attempt itself and
asks before allowing another ([`decisions.md`](decisions.md) entries 13 and 18).

A receiver is shown the transfer and asked before anything is written. In a
script, pass `--yes`: without a terminal, `drop recv` refuses to start.

```text
$ drop recv A1B2C3-zone-zoo-zebra --server http://127.0.0.1:8080 < /dev/null
error: there is no terminal to ask whether to accept this transfer. Pass --yes to accept whatever the sender sends without asking
```

Use synthetic files. Never point a test at the public instance.

## What a local transfer does not cover

The direct QUIC path *is* reachable from the binary — `--transport p2p` forces
it — but the commands above pass `--server`, so they use the relay. Two peers
on one machine also cannot show you much: a loopback or single-host run has no
NAT and no round-trip time, so hole punching, fallback and the acknowledgement
window are all unexercised however the transfer is carried. The QUIC tests
under `cargo test -p drop-cli --lib transport::quic` bind with relays disabled
and meet over loopback for the same reason.

Constructing a network that does exercise them is what
[`plans/network-lab-plan-2026-08-31.md`](plans/network-lab-plan-2026-08-31.md)
is for. See also the peer-to-peer plan under "What cannot be verified on the
development machine".

## Container

```bash
docker compose up --build
```

[`Dockerfile`](../Dockerfile) builds the relay. There is no other image.

## Kubernetes

See [`k8s/README.md`](../k8s/README.md). The GKE shutdown budget has its own
check:

```bash
scripts/check-gke-shutdown-budget.sh
```

## Metrics snapshot

```bash
curl -s http://127.0.0.1:8080/metrics
```

JSON, not Prometheus text. Zero `active_sessions` is what a clean shutdown or a
DNS cutover waits for.
