# Security Policy

## Supported versions

Drop is pre-release. Security fixes are applied to the current `main` branch;
older commits and independently operated deployments may not receive patches.

## Reporting a vulnerability

Do not open a public issue for an unpatched vulnerability. Use GitHub's
[private vulnerability reporting](https://github.com/op-q/drop/security/advisories/new)
flow.

A useful report includes:

- the affected revision and deployment shape;
- impact and the security boundary that failed;
- minimal reproduction steps using synthetic data;
- relevant browser, operating-system, or proxy details;
- a suggested mitigation, if you have one.

Maintainers should acknowledge a report within five business days and
coordinate remediation and disclosure privately.

## Useful report areas

- joining, replacing, or observing a transfer without its active code;
- session-code guessing or rate-limit bypass;
- transfer-size, connection, memory, or timeout limit bypass;
- cross-session data delivery;
- malicious filename or content-type handling;
- denial of service with a small number of requests;
- sensitive data exposure through logs, metrics, errors, or static assets;
- unsafe reverse-proxy behavior.

## Rules of engagement

- Do not access, change, retain, or destroy another person's data.
- Do not perform denial-of-service, spam, social engineering, or physical
  attacks.
- Do not scan a public Drop deployment without written permission.
- Use a local deployment, synthetic files, and the smallest proof necessary.
- If you encounter a credential or private file, stop and report it without
  using or redistributing it.

## Security model

Drop is a relay, not an end-to-end encrypted peer-to-peer protocol. The server
process handles file bytes in memory. HTTPS protects network hops when properly
configured, but a server operator or compromised host may observe an active
transfer. Session codes are short-lived capabilities and should be shared over
a trusted channel.
