# Drop

[![CI](https://github.com/op-q/drop/actions/workflows/ci.yml/badge.svg)](https://github.com/op-q/drop/actions/workflows/ci.yml)
[![CodeQL](https://github.com/op-q/drop/actions/workflows/codeql.yml/badge.svg)](https://github.com/op-q/drop/actions/workflows/codeql.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

No Drop server in the middle, and nothing readable in the middle when one is
involved. `drop send` and `drop recv` connect two computers directly over
QUIC. There is no hosted relay: a relay is something you run, and without one
the direct path is the only path.

> [!IMPORTANT]
> Drop is pre-release software. The protocol and deployment defaults may
> change, and the public instance should not be treated as a durable storage or
> high-assurance secure-transfer service.

## Install

```bash
curl -fsSL https://github.com/op-q/drop/releases/latest/download/install.sh | sh
```

## Use

```bash
$ drop send ./project
Sending ./project (128 files, archived as project.tar)
Looking for a peer-to-peer path...

  Run this on the other computer:

      drop recv 7F2A91-crossover-clockwork-ridge

Path    peer-to-peer (no Drop server)
Waiting for the receiver to connect...
```

```bash
$ drop recv 7F2A91-crossover-clockwork-ridge
Path    peer-to-peer (no Drop server)
Receiving  100.0%  412.7 MiB / 412.7 MiB  86.4 MiB/s  ETA --
Extracted 128 files into .
```

That's the default, `auto`: direct, falling back to a relay only if you have
configured one with `--server` or `DROP_SERVER`. Every transfer is encrypted
end to end either way — what the carrier changes is who moves the bytes, not
who can read them. Run `drop --help` for the full flag list, including
`--transport`, `--compress`, and `--force`.

"No Drop server" is the precise claim and not a larger one. The direct path
still finds the other computer through the public DHT and a relay operated by
n0, and that relay carries the encrypted connection when two peers cannot hole
punch. `DROP_RENDEZVOUS_RELAY` and `DROP_RENDEZVOUS_BOOTSTRAP` point both at
infrastructure you run instead.

## Docs

- [Security model](docs/security.md) — encryption, trust boundaries, hostile input
- [Architecture](docs/architecture.md) — crates, transport, transfer flow
- [Protocol](docs/protocol.md) — the wire format
- [Deployment](docs/deployment.md) — requirements, configuration, Docker, Kubernetes/GKE
- [Commands](docs/commands.md) — running from source, local dev workflows
- [Network lab](netlab/README.md) — topology tests, and what they do not prove
- [Contributing](.github/CONTRIBUTING.md)
- [Full documentation index](docs/README.md)

## License

Drop is available under the [MIT License](LICENSE).
