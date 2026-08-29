# Security model

Drop is an ephemeral relay. This document states what it protects, what it does
not, and which weaknesses are known and accepted. The root README carries a
short summary for users; this is the detailed version.

Drop is pre-release. Nothing here should be read as an assurance claim.

## Trust boundaries

| Party | What they can do |
| --- | --- |
| Network observer | Sees TLS-protected traffic when HTTPS is configured; sees connection metadata |
| Relay operator | Sees ciphertext, its length, the nameplate, and client IP addresses. Not file bytes, filenames, or MIME types |
| A third party holding an active code | May be able to join that session as its one receiver |
| The sending peer | Chooses every byte and every path inside an archive |
| The receiving peer | Chooses where bytes land, and whether to keep them |

The relay no longer sees plaintext. Both clients derive an AES-256-GCM key from
the secret half of the transfer code by SPAKE2, and that half never reaches the
relay; what crosses it is ciphertext, a byte count, and a nameplate that routes
the two peers together. See [`decisions.md`](decisions.md) entry 7.

**The CLI case and the browser case are not the same, and must never be
described in wording that blurs them.**

- **CLI to CLI is end-to-end encrypted.** The binary is fetched once, out of
  band, and the relay has no part in delivering it.
- **Browser transfers are encrypted in the browser, and are only as strong as
  the code the site delivered.** The page fetches its JavaScript and the
  WebAssembly envelope from the same origin as the relay, so an operator
  willing to serve modified client code can capture a transfer at the point
  where it is still plaintext. Compiling the envelope from the same Rust the
  CLI uses (entry 11) removes a class of implementation bugs; it does not
  remove this. What browser encryption does defeat is a passive operator, a
  compromised store of relayed traffic, and anyone who obtains the ciphertext
  later.

Do not describe Drop as peer-to-peer.

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

A code has two halves and they do different jobs:

```text
7F2A91-crossover-clockwork-ridge
^^^^^^ nameplate — six hex characters, the only half the relay is told
       ^^^^^^^^^^^^^^^^^^^^^^^^^ three words, 33 bits — the key-exchange
                                  password, which never leaves either client
```

The split is load-bearing. A relay given the password could run the exchange
against both peers at once and read everything, so the routing half and the
authenticating half have to be different bytes.

The nameplate is a temporary capability: whoever presents it first becomes the
session's one receiver. It no longer carries the payload's secrecy.

What bounds an attacker guessing codes:

- at most 100 sessions exist at once, so a random guess is unlikely to hit;
- a session lives at most five minutes without activity;
- a code is consumed by the first receiver to claim it, so a successful guess
  is visible — the real receiver is refused with `session already claimed`;
- per-IP limits cap connection attempts per minute.

What does not bound it: an attacker distributed across many source addresses.
The nameplate is small and the per-IP rate limit is the main thing in the way.

What makes that survivable is that guessing a nameplate no longer yields
readable bytes. An attacker who claims a session still has to know the three
words, and SPAKE2 gives them exactly one attempt: a wrong password produces a
different key, the sealed metadata fails to open, and claiming the session
consumed it. Guessing is online-only and non-repeatable, so 33 bits is measured
against a single try rather than an offline cracking rate.

The remaining cost of a guessed nameplate is denial of service — the attacker
burns the session and the real receiver is refused. Codes should still be
shared through a trusted channel.

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

- browser transfers are bounded by the code the site delivered, as above;
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
