# Drop

[![CI](https://github.com/op-q/drop/actions/workflows/ci.yml/badge.svg)](https://github.com/op-q/drop/actions/workflows/ci.yml)
[![CodeQL](https://github.com/op-q/drop/actions/workflows/codeql.yml/badge.svg)](https://github.com/op-q/drop/actions/workflows/codeql.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

No server in the middle when Drop can manage it, and nothing readable in the
middle even when there is one. `drop send` and `drop recv` connect two
computers directly over QUIC by default, falling back to a small encrypted
relay for browsers and uncooperative NATs.

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

That's the default, `auto` — direct when the two computers can reach each
other, the relay when they can't. Every transfer is encrypted end to end
either way. Run `drop --help` for the full flag list, including
`--transport`, `--compress`, and `--force`.

## Docs

- [Security model](docs/security.md) — encryption, trust boundaries, hostile input
- [Architecture](docs/architecture.md) — crates, transport, transfer flow
- [Protocol](docs/protocol.md) — the wire format
- [Deployment](docs/deployment.md) — requirements, configuration, Docker, Kubernetes/GKE
- [Commands](docs/commands.md) — running from source, local dev workflows
- [Contributing](.github/CONTRIBUTING.md)
- [Full documentation index](docs/README.md)

## License

Drop is available under the [MIT License](LICENSE).
