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

**CLI to CLI is end-to-end encrypted.** The binary is fetched once, out of
band, and the relay has no part in delivering it.

**No browser client ships** since 0.4.0 ([`decisions.md`](decisions.md) entry
17). The rule it lived under still binds any future one, and is kept here so it
is not rediscovered the hard way: **a browser transfer is encrypted in the
browser, and is only as strong as the code the site delivered.** A page that
fetches its JavaScript and envelope from an operator's origin lets that
operator, if willing to serve modified client code, capture a transfer where it
is still plaintext. Compiling the envelope from the same Rust the CLI uses
(entry 11) removes a class of implementation bugs; it does not remove this. What
browser encryption does defeat is a passive operator, a compromised store of
relayed traffic, and anyone who obtains the ciphertext later. Never describe the
two cases in wording that blurs them.

Do not describe Drop as a whole as peer-to-peer. The direct path is; a transfer
that falls back to the relay is not.

## What the relay does not do

- It does not write transferred file bytes to application storage.
- It does not retain a session after completion, cancellation, disconnect, or
  five minutes without activity.
- It does not send telemetry or upload anything externally.

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

### One guess, enforced twice over

"SPAKE2 gives them exactly one attempt" is a claim about the *code*, but
nothing in the envelope delivers it. Something outside has to, and which
something depends on the carrier. The guarantee is the same; the mechanism is
not, and confusing the two is how a path ships without either.

| | What holds an attacker to one guess | What a second guess costs them |
| --- | --- | --- |
| **Relay** | `claim_receiver` refuses a second claim on a session | a new session, which needs a new nameplate |
| **Direct** | the sender stops at the metadata checkpoint | a human approving it, one guess at a time |

Over the relay this is server-side and invisible to both peers. Over a direct
connection there is no server, so the sender does it: it sends nothing until
the peer proves it opened the sealed metadata, and a peer that fails — by
saying so, by timing out, by vanishing, or by claiming success it cannot prove,
which all count the same — consumes the transfer. The sender then asks the
person in front of it:

```text
A peer connected and failed the code.
This may be a mistype, or someone guessing.
Allow another attempt? [y/N]
```

This is stronger than a fixed retry limit and was chosen over one. An attacker
grinding the code needs a human approval per guess, which caps the attack at
human speed and — the part that matters more than the bits — *makes it
visible*: the attempt counter climbs where the sender's owner can see it. A
sender with no terminal allows nothing, so unattended use gets the strict
behaviour.

**The proof is what makes the prompt mean something.** Until protocol version
2 the peer's answer was a bare `meta_ok`, an assertion made by the party being
limited. A wrong guesser could send it anyway: the transfer was still consumed
and nothing readable leaked, but the attempt counter never climbed and nobody
was asked, so being probed was invisible. Since version 2 `meta_ok` carries a
32-byte key confirmation, a fourth HKDF output only a peer holding the same
keys can produce, and the sender checks it in constant time
(`SessionKeys::confirms`, in `crypto/`, so a later `==` cannot creep in at a
call site). Sending it reveals nothing: it does not lead back to the secret or
to the other keys, and the sender never sends its own copy, so there is
nothing to replay. It proves the receiver to the sender, not the other way
round, and needs no reverse proof: a sender without the keys could not have
sealed the metadata the receiver just opened. Recorded as
[`decisions.md`](decisions.md) entry 18.

Over the relay the same checkpoint runs, and a failure ends the transfer rather
than prompting, since the relay has already burned the session. There it is
what lets a sender know the code was right before a byte moves.

The denial of service above is unchanged by this and applies to both paths:
someone who guesses a nameplate can burn a transfer without learning anything.

The protocol side is in [`protocol.md`](protocol.md#the-metadata-checkpoint),
the reasoning and the rejected alternatives in
[`decisions.md`](decisions.md) entry 13.

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
- on Windows, a name component Windows would interpret is rewritten, never
  refused: `:` (which names an alternate data stream, or a drive), the other
  forbidden characters and control characters become `_`, trailing dots and
  spaces (which Windows strips) become `_`, and a device name such as `CON` or
  `nul.txt` gets `_` after its stem. A rewrite changes one component's spelling
  and cannot introduce a separator, so it cannot move an entry to a different
  directory. The existence and link checks run on the rewritten path, which is
  the one written. Every rewrite is reported. `cli/src/names.rs`;
- a component containing `\` is refused on every platform, because on Windows
  it would be a separator;
- an entry whose name the filesystem refuses, or a symlink the system cannot
  create, is skipped with a warning and extraction continues;
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
misleading the reader is the whole payoff — must render it inert first. A
terminal does not do this for you.

The CLI does this in `cli/src/display.rs`. Every name a peer chose, every
archive warning that quotes one, and every error message a peer or the relay
sent passes through it before reaching the terminal. The relay is untrusted, so
its messages count as peer text too. Control characters, including the bytes
that begin every escape sequence, and bidirectional and invisible formatting
characters are **replaced** with `U+FFFD`, not removed, so a doctored name looks
doctored. Whitespace runs collapse, so padding cannot push an extension out of
sight. Long names are shortened in the middle, keeping the extension. Only
display changes: a received file keeps the name its bytes arrived with, subject
to the path rules above. Until 2026-09-14 the receiver printed the sender's
filename verbatim in its `Receiving` line.

## Consent before bytes

A receiver used to learn what it was getting only as it arrived. Since protocol
version 2 it is shown the transfer first, and nothing is created until it
accepts:

- **The preview is built to be hard to lie with.** The name is sanitised (see
  above). The type label comes from the extension the file will actually have,
  not from the MIME type the sender chose, so `invoice.pdf` followed by padding
  and `.exe` shows as a program. A program extension on any of the three
  platforms adds a warning line. A folder's file count and unpacked size are the
  sender's claims and are labelled as such. The extractor's expansion limit,
  not the claim, bounds what is written.
- **Declining changes nothing on disk.** The destination is planned by looking,
  not creating, so no empty file and no reserved numbered name is left behind.
  A test compares the directory before and after.
- **The relay cannot accept on the receiver's behalf**, because `accept` is a
  receiver frame. It can refuse to forward one, which is denial of service, the
  same power it always had. It can also refuse to carry chunks before `accept`,
  and does.
- **Unattended use must be explicit.** Without a terminal, `drop recv` refuses to
  start unless given `--yes`. Scripts that piped `drop recv` before 0.4.0 need
  that flag, and the refusal names it.
- **Reasons are enumerated.** `decline` and `cancel` carry a word from a fixed
  set. The relay normalises them and each client maps them to its own sentence,
  so neither the relay nor a peer can put text on the other terminal that way.

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

## Self-hosted rendezvous

`DROP_RENDEZVOUS_RELAY` and `DROP_RENDEZVOUS_BOOTSTRAP` point the direct path at
infrastructure an operator runs instead of the public defaults. Unset, nothing
changes. [`decisions.md`](decisions.md) entry 15 records the decision; what an
operator takes on by setting them is this.

**A relay they name sees connection metadata** for every direct transfer that
uses it: which endpoint ids talk to each other, when, and the addresses each was
observed at. It sees no file bytes, no filename and no code — those are sealed by
the envelope, and choosing a relay does not weaken it. This is the same exposure
n0's relays have today, moved to a host the operator picked, and it is why a
relay URL is worth treating as infrastructure rather than as a preference.

**A bootstrap node they name can refuse to store a record, or serve a stale
one.** The result is a rendezvous that fails and a transfer that falls back to
the Drop relay. It cannot lead a receiver to the wrong peer: a rendezvous record
is signed by a key derived from the public nameplate, so anyone who can guess the
nameplate can produce a valid one, and the record was never evidence of identity.
Authentication is the PAKE's, as above.

**The sharp edge is a typo, not an attacker.** Setting these to keep rendezvous
inside a network and silently getting the public DHT instead loses precisely what
was being protected, and would look like nothing at all. So a malformed value is
an error rather than a fallback to the default, and a relay URL's scheme is
checked — `relay.example:3340` is a syntactically valid URL whose scheme is
`relay.example`, and accepting it would produce exactly that silent downgrade.

Neither variable relaxes the address filter. A record still carries only a
globally routable address or a relay URL, so pointing at a private relay
publishes the relay and still withholds every private address of the sender's.

## Known weaknesses

Recorded honestly rather than fixed:

- 24-bit session codes, as discussed above;
- no resume or retry, so a disconnect loses the transfer;
- release binaries are verified against checksums published in the same release,
  which detects corruption and truncation but not a compromised release. The
  trust anchor is GitHub; artifacts are not signed.

## Reporting

Report vulnerabilities privately according to the
[security policy](../.github/SECURITY.md). Do not open a public issue.
