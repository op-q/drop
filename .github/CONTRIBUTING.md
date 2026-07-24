# Contributing to Drop

Thanks for helping improve Drop. The project is pre-release and welcomes
focused bug fixes, tests, documentation, accessibility work, and carefully
scoped features.

## Before you start

1. Read the [README](../README.md), especially the privacy and security model.
2. Search existing issues and pull requests for overlapping work.
3. Open an issue before a substantial protocol, security, persistence, or
   deployment change so the behavior can be agreed first.
4. Never include real transferred files, credentials, private logs, IP
   addresses, or production data in a contribution.

## Workflow

1. Fork the repository and start from an up-to-date `main`.
2. Create a short-lived branch such as `fix/socket-cleanup`,
   `feat/transfer-feedback`, `docs/security-model`, or `ci/rust-cache`.
3. Make one coherent change.
4. Add tests for behavior changes and update documentation with the code.
5. Run the checks below.
6. Open a pull request targeting `main`.

You can optionally enable the repository's local pre-push guard:

```bash
git config core.hooksPath .githooks
```

## Validation

```bash
scripts/check-secrets.sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
npm --prefix web ci
npm --prefix web run build
npm --prefix web audit --audit-level=high
```

If a command could not be run, say why in the pull request.

## Pull requests

Explain:

- the user-visible outcome and why it matters;
- the implementation and important tradeoffs;
- the exact checks performed;
- privacy, security, compatibility, and deployment implications;
- follow-up work intentionally left out.

Keep commits narrow and use clear, plain-language messages. By contributing,
you agree that your contribution is licensed under Drop's
[MIT License](../LICENSE).

All contributors must follow the [Code of Conduct](CODE_OF_CONDUCT.md).
