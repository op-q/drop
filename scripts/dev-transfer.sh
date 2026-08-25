#!/usr/bin/env bash
#
# A whole transfer against a relay on this machine, using this checkout's
# binaries. No release download, no hosted relay, no network beyond loopback.
#
#   scripts/dev-transfer.sh            # round trip a generated file, verify bytes
#   scripts/dev-transfer.sh <PATH>     # round trip a file or folder of your own
#   scripts/dev-transfer.sh --relay    # just start the relay and print how to use it
#
# The two-terminal version, which is what you want when poking at it by hand:
#
#   cargo build --workspace
#   DROP_BIND_ADDR=127.0.0.1:8080 ./target/debug/api
#   DROP_SERVER=http://127.0.0.1:8080 ./target/debug/drop send ./some-file
#   DROP_SERVER=http://127.0.0.1:8080 ./target/debug/drop recv <CODE> -o /tmp/in

set -euo pipefail

PORT="${DROP_DEV_PORT:-8080}"
ORIGIN="http://127.0.0.1:${PORT}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v cargo >/dev/null || {
  echo "cargo is not on PATH. Install Rust, or point PATH at an existing toolchain." >&2
  exit 1
}

echo "==> building"
cargo build --workspace --bins

port_open() { (exec 3<>"/dev/tcp/127.0.0.1/${PORT}") 2>/dev/null; }

if port_open; then
  echo "==> something is already listening on ${PORT}; using it"
  STARTED_RELAY=""
else
  echo "==> starting a relay on ${ORIGIN}"
  DROP_BIND_ADDR="127.0.0.1:${PORT}" ./target/debug/api >"${TMPDIR:-/tmp}/drop-dev-relay.log" 2>&1 &
  STARTED_RELAY=$!
  for _ in $(seq 1 50); do
    port_open && break
    sleep 0.1
  done
  port_open || { echo "the relay never came up; see ${TMPDIR:-/tmp}/drop-dev-relay.log" >&2; exit 1; }
fi

cleanup() { [ -n "${STARTED_RELAY:-}" ] && kill "$STARTED_RELAY" 2>/dev/null || true; }
trap cleanup EXIT

if [ "${1:-}" = "--relay" ]; then
  echo
  echo "Relay is up. In two other terminals:"
  echo "    DROP_SERVER=${ORIGIN} ./target/debug/drop send <PATH>"
  echo "    DROP_SERVER=${ORIGIN} ./target/debug/drop recv <CODE>"
  echo
  echo "Ctrl-C to stop."
  wait "$STARTED_RELAY"
  exit 0
fi

WORK="$(mktemp -d)"
trap 'cleanup; rm -rf "$WORK"' EXIT
mkdir -p "$WORK/out"

if [ -n "${1:-}" ]; then
  SOURCE="$1"
else
  SOURCE="$WORK/generated.bin"
  head -c 3000000 /dev/urandom >"$SOURCE"
  echo "==> generated a 2.9 MiB file (over one chunk, so chunking is exercised)"
fi

echo "==> sending $(basename "$SOURCE")"
DROP_SERVER="$ORIGIN" ./target/debug/drop send "$SOURCE" >"$WORK/send.log" 2>&1 &
SEND=$!

CODE=""
for _ in $(seq 1 100); do
  CODE="$(grep -oE '[0-9A-F]{6}(-[a-z]+){3}' "$WORK/send.log" | head -1 || true)"
  [ -n "$CODE" ] && break
  sleep 0.1
done
[ -n "$CODE" ] || { echo "no code appeared:"; cat "$WORK/send.log"; exit 1; }
echo "==> code: $CODE"

DROP_SERVER="$ORIGIN" ./target/debug/drop recv "$CODE" -o "$WORK/out" --no-extract -f
wait "$SEND" || { echo "the sender failed:"; cat "$WORK/send.log"; exit 1; }

RECEIVED="$(find "$WORK/out" -type f | head -1)"
echo
if cmp -s "$SOURCE" "$RECEIVED"; then
  echo "OK — $(wc -c <"$SOURCE") bytes arrived identical"
else
  echo "MISMATCH between $SOURCE and $RECEIVED" >&2
  exit 1
fi
