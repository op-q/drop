# Security model

Drop is an ephemeral relay. This document states what it protects, what it does
not, and which weaknesses are known and accepted. The root README carries a
short summary for users; this is the detailed version.

Drop is pre-release. Nothing here should be read as an assurance claim.

## Trust boundaries

| Party | What they can do |
| --- | --- |
| Network observer | Sees TLS-protected traffic when HTTPS is configured; sees connection metadata |
| Relay operator | Sees file bytes, filenames, sizes, MIME types, session codes, and client IP addresses |
| A third party holding an active code | May be able to join that session as its one receiver |
| The sending peer | Chooses every byte and every path inside an archive |
| The receiving peer | Chooses where bytes land, and whether to keep them |

The relay is a trusted component today. It handles plaintext file bytes in
memory while relaying them, so the operator and a compromised server can access
an active transfer. Removing that is the goal of the encryption work in
[`implementation-checklist.md`](implementation-checklist.md).

Do not describe Drop as peer-to-peer or end-to-end encrypted while this is true.

## What the relay does not do

- It does not write transferred file bytes to application storage.
- It does not retain a session after completion, cancellation, disconnect, or
  five minutes without activity.
- It does not send telemetry, upload anything externally, or make third-party
  browser requests.

The no-storage property is an application guarantee. Operating-system, proxy,
and infrastructure behavior is outside it — a kernel buffer, a swap file, or an
intermediate proxy is not something the application controls.

## Session codes

A code is six uppercase hexadecimal characters taken from a UUIDv4, so roughly
24 bits. It is a temporary capability: whoever presents it first becomes the
session's one receiver.

What bounds an attacker guessing codes:

- at most 100 sessions exist at once, so a random guess is unlikely to hit;
- a session lives at most five minutes without activity;
- a code is consumed by the first receiver to claim it, so a successful guess
  is visible — the real receiver is refused with `session already claimed`;
- per-IP limits cap connection attempts per minute.

What does not bound it: an attacker distributed across many source addresses.
24 bits is small, and the per-IP rate limit is the main thing standing in the
way. This is accepted for now because the window is short and a hit is loud,
but it is a real weakness and it is the reason codes should be shared through a
trusted channel.

Encryption changes this materially: once the key lives beside the code rather
than in it, guessing a code no longer yields readable bytes.

## Hostile input from a peer

The receiving end treats an archive as hostile, because the sender chooses every
path inside it:

- absolute paths, `..` components, and Windows drive prefixes are refused;
- an entry is refused if any parent directory on disk is a symbolic link, which
  is what stops a chain of links from walking the extractor out of the
  destination even when every path is lexically clean;
- a symlink is refused if its target leaves the destination, evaluated against
  what is on disk rather than against the target's text;
- existing files are kept unless `--force` is given;
- permission bits are masked to ownership bits, so an archive cannot set setuid,
  setgid, or sticky;
- a compressed payload that expands more than a hundredfold is abandoned, which
  bounds a decompression bomb by what the sender had to push through the relay.

Path safety must be judged against the filesystem, not only against path text: a
lexical check alone misses an entry that escapes through a symlink an earlier
entry created.

A filename is also hostile input for *display*. It is chosen by the sender and
may contain control characters, ANSI escape sequences, or bidirectional
overrides. Any surface that renders it — especially a confirmation prompt, where
misleading the reader is the whole payoff — must render it inert first. The web
client escapes by default; a terminal does not.

## Resource bounds

Every bound exists to stop one peer consuming the relay:

- a 4 GiB transfer limit, checked at session creation and again against `meta`;
- 100 concurrent sessions;
- four WebSocket connections per IP, so one address can run two transfers;
- per-IP session-creation and connection-attempt rate limits;
- a frame ceiling slightly above the 1 MiB chunk size;
- one 200 MiB server-wide ceiling on buffered file data, shared across sessions
  rather than multiplied by them;
- a 45-second socket idle timeout and a five-minute session lifetime.

A reservation against the buffer ceiling is returned when its chunk reaches the
receiver and also when a session is discarded, so an abandoned transfer cannot
strand capacity.

## Sensitive data

Treat as sensitive: active session codes, transferred bytes, filenames, client
IP addresses, and operational logs. Do not add any of these to documentation,
fixtures, issues, or pull request descriptions. Use synthetic values.

Per-IP limits depend on the client address being correct, so a reverse proxy
must preserve the intended client-address semantics. `DROP_TRUST_GCP_X_FORWARDED_FOR`
exists for one specific trusted-load-balancer arrangement and should not be
enabled otherwise: trusting a forwarded address that any client can set turns
the per-IP limits off.

## Known weaknesses

Recorded honestly rather than fixed:

- the relay sees plaintext;
- 24-bit session codes, as discussed above;
- no resume or retry, so a disconnect loses the transfer;
- release binaries are verified against checksums published in the same release,
  which detects corruption and truncation but not a compromised release. The
  trust anchor is GitHub; artifacts are not signed;
- browsers without direct-to-disk download support buffer the whole file in
  memory, capped at 256 MiB by the web client.

## Reporting

Report vulnerabilities privately according to the
[security policy](../.github/SECURITY.md). Do not open a public issue.
