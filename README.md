# Drop

[![CI](https://github.com/op-q/drop/actions/workflows/ci.yml/badge.svg)](https://github.com/op-q/drop/actions/workflows/ci.yml)
[![CodeQL](https://github.com/op-q/drop/actions/workflows/codeql.yml/badge.svg)](https://github.com/op-q/drop/actions/workflows/codeql.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Drop is a command-line tool for sending files and folders between computers. Run
`drop` and choose send or receive. The sender gets a code to give to the
receiver.

Transfers are end-to-end encrypted and go directly between the two computers. If
a direct connection isn't possible, they fall back to a relay: by default a
public one run by n0, the makers of iroh, or one you host yourself. The relay
only sees encrypted data.

Works across Linux, macOS and Windows.

> [!NOTE]
> Drop is early software. Use the same version on both computers.

## Install

Linux and macOS:

```bash
curl -fsSL https://github.com/op-q/drop/releases/latest/download/install.sh | sh
```

Windows, in PowerShell:

```powershell
irm https://github.com/op-q/drop/releases/latest/download/install.ps1 | iex
```

## Use

```bash
drop
drop send [filepath]
drop recv [code]
```

## More

- [Documentation](docs/README.md): security, protocol, and running your own relay
- [MIT License](LICENSE)
