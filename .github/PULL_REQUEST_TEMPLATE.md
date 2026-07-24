## Outcome

<!-- What changes for a Drop user, operator, or contributor? -->

## Implementation

<!-- Summarize the approach and important tradeoffs. -->

## Verification

<!-- List exact commands and manual checks. Explain anything not run. -->

- [ ] `scripts/check-secrets.sh`
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --all-targets`
- [ ] `npm --prefix web ci && npm --prefix web run build`
- [ ] `npm --prefix web audit --audit-level=high`

## Security, privacy, and operations

- [ ] I reviewed the complete diff for credentials and private data.
- [ ] I used only synthetic transfer files, logs, addresses, and fixtures.
- [ ] Documentation and tests match the actual behavior.
- [ ] New persistence, logging, proxy, resource, or deployment implications are
      described below.

<!-- Describe risks, rollout steps, compatibility impact, or write "none". -->
