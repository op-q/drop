#!/usr/bin/env bash
# Vercel's build image is Node-only, and this client is no longer a Node-only
# build: `npm run build` compiles `crypto-wasm/` so the browser runs the same
# envelope the CLI runs rather than a second implementation of it. See
# docs/decisions.md entry 11.
#
# Installing a Rust toolchain here is the cost of that decision. It is paid on
# every cold build, which is why the toolchain is minimal and wasm-pack is
# downloaded rather than compiled.
set -euo pipefail

WASM_PACK_VERSION="0.15.0"
WASM_PACK_SHA256="c09f971ecaed9a2efc80fdcea7a00ef6b53c7fadc8c57d1f61b53a6aa66b668a"

echo "--> installing Rust (minimal, wasm32 target only)"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --profile minimal --default-toolchain stable \
       --target wasm32-unknown-unknown
# shellcheck disable=SC1091
. "$HOME/.cargo/env"

echo "--> installing wasm-pack ${WASM_PACK_VERSION}"
url="https://github.com/rustwasm/wasm-pack/releases/download/v${WASM_PACK_VERSION}/wasm-pack-v${WASM_PACK_VERSION}-x86_64-unknown-linux-musl.tar.gz"
curl -fsSL -o /tmp/wasm-pack.tar.gz "$url"
echo "${WASM_PACK_SHA256}  /tmp/wasm-pack.tar.gz" | sha256sum -c -
mkdir -p /tmp/wasm-pack && tar -xzf /tmp/wasm-pack.tar.gz -C /tmp/wasm-pack --strip-components=1
install -m 0755 /tmp/wasm-pack/wasm-pack "$HOME/.cargo/bin/wasm-pack"

echo "--> npm ci"
npm ci

echo "--> toolchain ready: $(cargo --version), $(wasm-pack --version)"
