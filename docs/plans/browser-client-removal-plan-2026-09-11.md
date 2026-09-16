# Browser client removal plan

Status: **done** — phase 0 and phases 1–4 landed 2026-09-14; recorded as decisions entry 17
Created: **2026-09-11**
Last updated: **2026-09-14**

## Goal

Delete `web/` and `crypto-wasm/`, and every dependent that assumes a browser
client exists — the server routes that serve it, the CI job that builds it, the
Docker stage that bundles it, and the documentation that describes it.

**The relay stays.** This plan removes the browser client, not `api`. The two
are easy to confuse because [`../decisions.md`](../decisions.md) entry 16 left
the relay with only one stated reason to exist, and that reason was the browser.
It has another one, it is tested, and [What does not change](#what-does-not-change)
argues it.

## Why now

Four reasons, in descending order of how much they would survive an argument.

**1. Entry 16 already removed the browser client's audience.** With
`DEFAULT_SERVER` deleted and no hosted relay, a browser user has nothing to
connect to out of the box. The only person who can use `web/` today is someone
who self-hosts `Dockerfile.fullstack` — that is, someone who has already built a
deployment. Everyone else reaches a client that cannot reach a relay.

**2. It has never been exercised by a browser.** This is the repository's own
admission, not a new finding —
[`../implementation-checklist.md`](../implementation-checklist.md) item 2
records it: *"`App.svelte` itself. `tsc` does not check `.svelte` files and the
interop tests drive the envelope and the protocol from Node, not the UI. The
browser flows have been exercised by the build and by their shared envelope, not
by a browser."* An untested second implementation of the protocol is a liability
whether or not anyone uses it.

**3. It costs every week.** The `web` CI job installs Node, a pinned
`wasm-pack`, and the `wasm32-unknown-unknown` target, then builds the workspace
binaries again for the interop test — the single most expensive job in
[`../../.github/workflows/ci.yml`](../../.github/workflows/ci.yml) at a
25-minute timeout against 15 for Rust. It generates dependabot traffic that is
pure noise once the client is gone (PR #61 is open at the time of writing). It
holds four items in [`release-checklist.md`](../release-checklist.md) that
nobody can honestly tick.

**4. It costs almost nothing against the future.** The obvious counter-argument
is that a browser client is worth having and deleting it forfeits the work. It
mostly does not: see
[`browser-on-iroh-plan-2026-09-11.md`](browser-on-iroh-plan-2026-09-11.md),
which would replace `web/src/api.ts` (a WebSocket client to a relay this plan
keeps but that plan stops using) and `web/src/envelope.ts` (a wrapper around
`crypto-wasm`) in their entirety. What survives a rebuild is UI judgement, and
that is recoverable from git history at any time.

## The branch that already exists

`chore/remove-web` at `f30bcfb`, one commit, 22 files, 3,819 deletions, **28
commits behind `main`**. No pull request is open for it.

Its commit message is accurate about its own scope: *"Delete the browser client,
and nothing that depended on it yet."* Every dependent below is still present on
that branch, and **merging it as it stands breaks the next release** — see
phase 0.

Either rebase it onto `main` and build the phases on top, or restart from `main`
and let it go. Rebasing keeps the deletion reviewable as one commit, which is
worth something; the branch is old enough that either is defensible.

## What is being removed

| Path | What it is | Action |
| --- | --- | --- |
| `web/` | Svelte and TypeScript browser client | delete |
| [`../../crypto-wasm/`](../../crypto-wasm/) | `drop-crypto` compiled to WebAssembly, bindings only | delete |
| [`../../Dockerfile.fullstack`](../../Dockerfile.fullstack) | builds the frontend and the server into one image | delete |
| `web/public/install.sh` | the CLI installer | **move**, see phase 0 |

## What does not change

- **The relay.** `api`, `src/ws/`, the session store, the rate limiter, the
  budget accounting: all stay. Its browser justification is gone; its other one
  is real and tested. `--transport relay` is what works inside a network where
  UDP never gets out, `netlab`'s `udp_blocked` topology covers exactly that, and
  entry 16 explicitly kept `DROP_SERVER` as the way a deployment runs its own.
  Deleting the browser must not be allowed to drift into deleting the relay —
  that is a separate decision with a separate argument, and this plan does not
  make it.
- **`drop-crypto`.** (Corrected 2026-09-14: `api` does not depend on it. It repeats the constants, guarded by `cli/tests/protocol.rs`.) The CLI depends on it for
  the envelope. Only the wasm *bindings* crate goes.
- **The wire.** No protocol change, no framing change, no envelope change. A
  `drop` binary built before this lands interoperates with one built after it.
  This is the one reason the removal can ship independently of everything else
  on the checklist.
- **The four release targets** and the install path. Phase 0 moves the script;
  it does not change what it does or where a user gets it.
- **`DROP_RENDEZVOUS_*`, `--transport`, `--status`.** Untouched.

## Phase 0 — move `install.sh` before anything is deleted

**This phase is release-critical and lands on its own, ahead of the rest.**

[`release.yml`](../../.github/workflows/release.yml) sparse-checks-out
`web/public/install.sh` at line 130 and publishes it at line 153 under
`fail_on_unmatched_files: true` at line 154. Deleting `web/` without moving the
script first means the next `v*` tag fails in the `publish` job — after the
build matrix has succeeded, so the failure arrives late and looks unrelated to
the commit that caused it.

The script has nothing to do with the browser client. It downloads binaries from
GitHub releases and verifies them against `checksums.txt`; it lived under
`web/public/` only because entry 8's split deployment served it as a static file
from the frontend host. Entry 16 ended that arrangement.

- [x] Move `web/public/install.sh` to `scripts/install.sh`. `web/dist/install.sh`
      is the committed build output of the same file and goes with `web/` in
      phase 2.
- [x] Update [`release.yml`](../../.github/workflows/release.yml) lines 123, 130
      and 153 to the new path.
- [x] Remove the `/install.sh` route at
      [`src/lib.rs:47-49`](../../src/lib.rs#L47-L49). The relay served it so
      `curl https://drop.lifbom.com/install.sh` worked; that host is gone, and
      [`../../README.md`](../../README.md) already points at
      `github.com/op-q/drop/releases/latest/download/install.sh`.
- [x] Check the installer still resolves: `DROP_VERSION=v0.3.0 sh scripts/install.sh`
      into a scratch `DROP_INSTALL_DIR`.

Done 2026-09-14 on `chore/move-install-script`. `scripts/install.sh` keeps mode
`100755` and is byte-identical to the v0.3.0 release asset (`cmp` against the
download). `DROP_VERSION=v0.3.0` installed a binary that reports `drop 0.3.0`.
180 tests pass, the same count as before, since no test covered the route.
`docs/deployment.md`'s split-deployment note named the relay route as the
installer's source and now names the release asset. The publish job cannot be
exercised without tagging, so the three `release.yml` edits were checked by
reading: the sparse checkout path, the comment, and the `files:` entry all name
`scripts/install.sh`, and no `web/public` reference remains.

Landing this alone, before any deletion, means the release path is never broken
even for one commit — and if the rest of the plan stalls, nothing is left
half-done.

## Phase 1 — the server stops serving a browser

- [x] Delete the routes at [`src/lib.rs:43`](../../src/lib.rs#L43) (`/` →
      `web/dist/index.html`) and [`src/lib.rs:57`](../../src/lib.rs#L57)
      (`/assets` → `web/dist/assets`). The `/install.sh` route went in phase 0.
- [x] Drop the now-unused `ServeDir` / `ServeFile` imports at
      [`src/lib.rs:28`](../../src/lib.rs#L28), and `get_service` from the
      `axum::routing` import at [`src/lib.rs:19`](../../src/lib.rs#L19) if
      nothing else uses it.
- [x] Delete `index_serves_the_drop_entrypoint` at
      [`tests/health_check.rs:59-76`](../../tests/health_check.rs#L59-L76). It
      asserts the response body contains `<div id="app"></div>` and `./assets/`,
      both of which are the Svelte entrypoint.
- [x] Decide what `GET /` returns now. It must not 404 silently into a
      monitoring gap — `/health` and `/ready` exist for probes, so the honest
      options are a 404 with a one-line body naming the project, or a redirect
      to the repository. See [open question 2](#open-question-2--what-does-get--return).

## Phase 2 — delete the client and its wasm bindings

- [x] Delete `web/`.
- [x] Delete `crypto-wasm/`.
- [x] Remove `crypto-wasm` from `members` at
      [`Cargo.toml:2`](../../Cargo.toml#L2).
- [x] Delete [`.cargo/config.toml`](../../.cargo/config.toml). Its only content
      is the `wasm32-unknown-unknown` `getrandom_backend` rustflag, and its own
      comment says it is scoped to that target on purpose. With no wasm target
      in the workspace the file has no remaining job. **Check first** that
      nothing else was added to it since this plan was written.
- [x] `cargo update --workspace` or equivalent so `Cargo.lock` drops
      `drop-crypto-wasm` and the wasm-only dependency tree under it.

## Phase 3 — build, CI, and container surface

- [x] Delete the `web` job, [`ci.yml:51-102`](../../.github/workflows/ci.yml#L51-L102).
      That removes the Node setup, the pinned `wasm-pack` download, the wasm32
      target, `npm audit`, and the extra `cargo build --workspace --bins` that
      existed to stop the interop test skipping silently.
- [x] Delete [`Dockerfile.fullstack`](../../Dockerfile.fullstack) and point
      [`docker-compose.yml:5`](../../docker-compose.yml#L5) at
      [`Dockerfile`](../../Dockerfile), which already builds `api` alone and
      needs no change.
- [x] Remove `web/node_modules` and `web/dist` from
      [`.dockerignore:6-7`](../../.dockerignore#L6-L7).
- [x] Remove the web entries from [`.gitignore:9-12`](../../.gitignore#L9-L12)
      and fix the section comment at line 1 (`# Rust and web build state`).
- [x] Remove `*.svelte text` at
      [`.gitattributes:4`](../../.gitattributes#L4) and the Vite whitespace
      exemption at lines 8-10. Keep `*.ts text` only if any TypeScript remains;
      after this plan, none does.

## Phase 4 — documentation

The largest phase by file count and the easiest to leave half-done. Every item
here is a claim that becomes false the moment phase 2 lands, and
[AGENTS.md](../../AGENTS.md) requires README claims to track tested behavior.

- [x] [`AGENTS.md`](../../AGENTS.md): delete the web build rule and the
      `tsc`/`.svelte` rule at lines 71-74, and the five `npm --prefix web`
      commands at lines 86-90. **The two claim invariants at lines 33-40 are a
      separate question** — see [open question 3](#open-question-3--the-claim-invariants).
- [x] [`architecture.md`](../architecture.md): drop the `crypto-wasm/` and
      `web/` rows from the crate table (lines 47-48), redraw the dependency
      diagram at lines 55-56, drop the Browser row from the transfer-work table
      at line 87, and reword line 28 — *"It stays as the fallback for browsers,
      for UDP-blocked networks, and for the NAT cases hole-punching cannot
      solve"* — to the two reasons that survive. Line 37's "four members plus a
      web client" becomes three members.
- [x] [`commands.md`](../commands.md): delete the npm block at lines 14-18, the
      wasm toolchain setup at lines 27-38, the interop note at lines 40-41, the
      second npm block at 65-66, and the Vite dev-server section at 78-81.
- [x] [`deployment.md`](../deployment.md): drop Node.js and npm from
      Requirements (lines 10-11), fix "Run it from source" (line 17) to
      `cargo run` alone, remove `VITE_BACKEND_ORIGIN` from the configuration
      table (line 38), rewrite the Docker section (lines 81-83), and delete the
      "Split deployment" section (lines 89-100) — a frontend/backend split with
      no frontend is not a shape anyone can deploy.
- [x] [`release-checklist.md`](../release-checklist.md): delete the three npm
      commands (lines 33-35) and the `npm audit` item (42), and the browser
      smoke tests at lines 54, 58 and 62. Line 62's direct-to-disk item refers
      to the File System Access API and goes with them.
- [x] [`security.md`](../security.md): lines 24-47 are the CLI-versus-browser
      section and lines 223-229 the known-weaknesses entries that depend on it.
      Subject to [open question 3](#open-question-3--the-claim-invariants).
- [x] [`docs/README.md`](../README.md): line 106's browser caveat.
- [x] [`k8s/README.md`](../../k8s/README.md): lines 78-83 justify
      `WS_MAX_MESSAGE_BYTES` by *"The browser client sends 64 KiB chunks, so the
      cap leaves four times the headroom it needs."* **That paragraph is already
      wrong and this is a good moment to fix it**: the constant is
      `RECOMMENDED_CHUNK_BYTES + 64 * 1024` at
      [`src/config.rs:47`](../../src/config.rs#L47) — 1 MiB + 64 KiB, not the
      256 KiB the text claims. Re-justify it against the CLI's 1 MiB chunk. No
      code change; the cap is already keyed to the shared constant rather than
      to the browser.
- [x] [`README.md`](../../README.md): no change needed. Its only match on
      "browser" is *"a file browser for `send`"*, which is the terminal UI.
      Confirm rather than assume.
- [x] [`decisions.md`](../decisions.md): add **entry 17**, recording the removal
      and its reasoning. Mark **entry 11** (the browser runs the envelope as
      WebAssembly) superseded, the way entry 8 was marked by 16, rather than
      deleting it. Entry 11 is the record of why `crypto/` is a separate crate,
      and `crypto/Cargo.toml:15`, `cli/src/lib.rs:9` and `cli/tests/protocol.rs:57`
      all cite that reason in comments — they need rewording to say the split is
      kept for the envelope's own sake and for whatever compiles it next.

## Phases 1–4, done 2026-09-14 — what the plan missed

Landed together on `chore/remove-browser-client`, stacked on phase 0 and the
cross-platform CI change. The deletions were as listed. Found on the way:

- **`Dockerfile` did not build, and neither did `Dockerfile.fullstack`.** Both
  copied `Cargo.toml`, `cli/Cargo.toml` and `src/`, but not `crypto/`, and cargo
  loads every workspace member's manifest, and each member's path dependencies,
  before building any one of them. The relay itself does not depend on
  `crypto/`: it repeats the constants it enforces, and `cli/tests/protocol.rs`
  guards the copies. `docs/architecture.md` said otherwise, and is corrected. The
  plan said `Dockerfile` "already builds `api` alone and needs no change". That
  was wrong, and had been since the envelope became its own crate. Checked
  without Docker by copying exactly the files the Dockerfile copies into a
  scratch directory: `cargo metadata` failed on `drop-crypto`. With `crypto/`
  added, `cargo build --release --locked --package api` succeeded there. The
  image itself was not built, since Docker is not installed on this machine.
- **Branch protection requires "Web" and "Analyze JavaScript and TypeScript".**
  Deleting the `web` job makes the first never report, so every pull request
  would wait on it. Not in the plan. It is a repository settings change.
- **CodeQL analysed JavaScript and TypeScript only.** With no TypeScript left
  that job would scan nothing. It now analyses Rust (`build-mode: none`) under
  the name "Analyze Rust", which is the second required-check rename.
- **`.github/dependabot.yml` had an npm entry for `/web`**, and the GitHub
  templates (`CONTRIBUTING.md`, the pull request template, the bug report's
  "Browser client" surface, `SECURITY.md`) listed npm commands and browser
  details. All updated. None were in the plan's file list.
- **`k8s/README.md` was wrong in more ways than the plan said.** It named the
  cap as 256 KiB and justified it by a browser chunk size. The cap is 1 MiB plus
  64 KiB, and the bound that actually keeps the pod inside its memory limit is
  `RELAY_BUDGET_BYTES`. Both build commands used `Dockerfile.fullstack`.
- **`AGENTS.md` said "every transfer today crosses the relay"**, which has been
  false since the direct path shipped in v0.2.0. Rewritten along with the
  dormant browser rules.
- `cli/src/main.rs`'s `--transport` help told users a browser peer needs a
  relay. Removed, along with the comments in `cli/Cargo.toml`,
  `crypto/Cargo.toml`, `cli/src/lib.rs`, `crypto/src/lib.rs`,
  `cli/src/direct.rs`, `cli/src/transport/relay.rs`, `cli/tests/protocol.rs`,
  `netlab/README.md`, `netlab/runner.py` and the shakeout template.

Validation: 193 tests (the removed `index_serves_the_drop_entrypoint` is replaced
one-for-one by `the_root_explains_what_this_host_is`). `git grep` for `web/`,
`svelte`, `wasm-pack`, `vite` and `npm ` finds nothing outside `docs/plans/`,
`decisions.md` and the checklist's history, except `deployment.md`'s
removal note. Not run here: `docker compose up --build` (no Docker), the
Kubernetes render (CI's `kubernetes` job covers it), netlab (the relay code it
drives is untouched apart from the removed routes and CORS layer), and a
relayed CLI transfer as a separate manual check, because the end-to-end tests
already drive real relayed transfers and pass.

## Risks

**The release breaks if phase 0 is skipped or reordered.** The whole reason
phase 0 exists. `fail_on_unmatched_files: true` means the failure is loud, but
it lands in the `publish` job after a full build matrix has run.

**Self-hosters of `Dockerfile.fullstack` lose their browser UI.** This is the
one genuinely breaking change for a real user, and it deserves release notes
saying so plainly rather than a line in a changelog. It is a minor-version
change: no wire change, no CLI change, a removed deployment shape.

**UI work is discarded.** Mitigated but worth stating: `App.svelte` is 1,072
lines and recoverable from `main` at `a1e84d6` or from `f30bcfb^`. Name that
revision in entry 17 so a future reader does not have to bisect for it.

**Scope drift into deleting the relay.** Called out in
[What does not change](#what-does-not-change) because it is the plausible
mistake here, not a theoretical one — entry 16 genuinely did remove the relay's
*stated* reason to exist, and a reader who finds that entry first will conclude
the relay is next.

## Validation

Run the full set from [`commands.md`](../commands.md), less the npm half that
this plan deletes:

```bash
scripts/check-secrets.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

- [ ] The workspace builds and tests green with two members, not three. Record
      the test count; it should drop by exactly one (`index_serves_the_drop_entrypoint`).
- [ ] `git grep -n 'web/'` returns nothing outside `docs/plans/` and the
      superseded `decisions.md` entries. Historical records keep their
      references; live documentation does not.
- [ ] `git grep -ni 'svelte\|wasm-pack\|vite\|npm '` returns nothing outside
      those same two places.
- [ ] `docker compose up --build` starts and `/health` answers.
- [ ] `kubectl kustomize k8s/overlays/local` and `.../gke` still render — the
      `kubernetes` CI job covers this, but the manifests reference the image
      built by the compose file.
- [ ] `netlab` still passes. It drives the real binaries and the real relay and
      never touched the browser, so this should be a no-op — which is the point
      of checking.
- [ ] A CLI-to-CLI transfer over `--transport relay` completes. The relay is the
      thing most at risk of being damaged by a plan about removing something
      else.

**Gate:** the workspace is green with `web/` and `crypto-wasm/` gone, a relayed
CLI-to-CLI transfer still completes, and a release built from the resulting tree
publishes `install.sh` from its new path. The third is the one that cannot be
checked by running tests, so check it against the workflow file by hand before
tagging.

## Open questions

### Open question 1 — does `DROP_ALLOWED_ORIGINS` survive?

CORS has exactly one consumer today and it is the browser client.
[`src/config.rs:123-137`](../../src/config.rs#L123-L137) builds the layer,
[`src/lib.rs:65`](../../src/lib.rs#L65) applies it, and nothing else sends an
`Origin` header — the CLI does not. After this plan it is configuration for
nobody, documented in a table that
[`release-checklist.md`](../release-checklist.md) requires to list every
variable the code reads.

Note that it stays dead under
[`browser-on-iroh-plan-2026-09-11.md`](browser-on-iroh-plan-2026-09-11.md) too:
a browser iroh node talks to an *iroh* relay, not to `api`, so it would not make
a cross-origin request to this server either.

Leaning: remove the layer and the variable, and say so in entry 17. Against:
someone may be serving their own frontend against a self-hosted relay today, and
this silently breaks them. Cheap compromise: keep it, and let the table say it
has no in-tree consumer.

**Answered 2026-09-14, at the user's direction: remove it.** The layer and the
variable both go, and entry 17 and the release notes say so, so a self-hoster
serving their own frontend finds out from the notes rather than from a browser
console.

### Open question 2 — what does `GET /` return?

Options: 404 with a one-line body naming the project and linking the repository;
a 308 to the repository; or leaving the route absent so axum's default 404
answers. A bare unstyled 404 at the root of a self-hosted relay reads as a
broken deployment, which matters because the operator is the only person who
will ever see it.

Leaning: a 404 with a short plain-text body. It is honest, it needs no new
dependency, and it cannot be mistaken for a redirect loop.

**Answered 2026-09-14: the leaning.** `GET /` answers 404 with one plain-text
line naming the project and the repository.

### Open question 3 — the claim invariants

[AGENTS.md](../../AGENTS.md) lines 33-40 and
[`security.md`](../security.md) lines 24-47 carry two rules that exist because a
browser client exists: *"Browser transfers never qualify"* for the
peer-to-peer claim, and *"Browser transfers are encrypted in the browser but are
only as strong as the code the site delivered."*

With no browser client, both are rules about nothing. Deleting them is tidy and
is also how the claim gets made wrong the next time somebody ships a browser
client — and
[`browser-on-iroh-plan-2026-09-11.md`](browser-on-iroh-plan-2026-09-11.md) is
that next time, where **both rules still hold unchanged**, because a browser
still runs code the site delivered no matter which transport carries the bytes.

Leaning: keep both, reworded to name their dormancy — one sentence saying no
browser client currently ships and that these bind any future one. A rule that
survives the thing it described is cheaper than rediscovering it.

**Answered 2026-09-14: the leaning.** Both rules stay, reworded as dormant.

### Open question 4 — which release

The removal is not a wire change, so it can ship alone as `0.4.0`. `meta_ok`
key confirmation *is* a wire change and is the other candidate for that version.
Shipping them together gives users one disruptive upgrade instead of two;
shipping the removal alone gets a smaller change in front of people sooner.

Not this plan's decision, but it should be made before either is tagged.

**Answered 2026-09-14: together, as `0.4.0`.** The removal ships with `meta_ok`
key confirmation and with the receiver consent and status work in
[`receiver-consent-and-status-plan-2026-09-14.md`](receiver-consent-and-status-plan-2026-09-14.md),
which is a wire change too. One disruptive upgrade instead of three. Doing the
removal first also deletes phase 3 of the `meta_ok` plan, which is browser work.

## Kickoff prompt

> Read `docs/plans/browser-client-removal-plan-2026-09-11.md` and
> `AGENTS.md`. Verify its file and line references against the current source
> before relying on them — the plan records intent at writing time. Start with
> phase 0 only, on a topic branch off `main`, and stop for review before phase 1:
> phase 0 is release-critical and is meant to be reviewable and mergeable alone.
> Answer open question 1 and 2 before phase 1, and open question 3 before
> phase 4.
