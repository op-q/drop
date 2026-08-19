#!/bin/sh
# Installer for the Drop command-line client.
#
#   curl -fsSL https://drop.lifbom.com/install.sh | sh
#
# Downloads a prebuilt binary from the project's GitHub releases, verifies it
# against the published checksums, and installs it.
#
#   DROP_INSTALL_DIR   where the binary lands (default: ~/.local/bin)
#   DROP_VERSION       release tag to install (default: latest)
#   DROP_RELEASE_BASE  base URL holding the release assets, for self-hosted
#                      mirrors of the published binaries
set -eu

REPO="op-q/drop"
VERSION="${DROP_VERSION:-latest}"
INSTALL_DIR="${DROP_INSTALL_DIR:-${HOME}/.local/bin}"

say() { printf '%s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "this installer needs $1, which is not on PATH"
}

detect_target() {
    kernel=$(uname -s)
    machine=$(uname -m)

    case "${machine}" in
        x86_64 | amd64) arch="x86_64" ;;
        aarch64 | arm64) arch="aarch64" ;;
        *) die "unsupported architecture: ${machine}" ;;
    esac

    case "${kernel}" in
        Linux) printf '%s-unknown-linux-musl' "${arch}" ;;
        Darwin) printf '%s-apple-darwin' "${arch}" ;;
        *)
            die "unsupported operating system: ${kernel}. Build from source with: cargo install --git https://github.com/${REPO} drop-cli"
            ;;
    esac
}

# Verifies a file against a checksums list, using whichever tool exists.
verify_checksum() {
    file_name="$1"
    checksums="$2"

    expected=$(awk -v name="${file_name}" '$2 == name || $2 == "*" name { print $1 }' "${checksums}")
    [ -n "${expected}" ] || die "no checksum published for ${file_name}"

    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "${file_name}" | awk '{ print $1 }')
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "${file_name}" | awk '{ print $1 }')
    else
        die "this installer needs sha256sum or shasum to verify the download"
    fi

    [ "${expected}" = "${actual}" ] ||
        die "checksum mismatch for ${file_name}: expected ${expected}, got ${actual}"
}

need curl
need tar

target=$(detect_target)
archive="drop-${target}.tar.gz"

if [ -n "${DROP_RELEASE_BASE:-}" ]; then
    base="${DROP_RELEASE_BASE%/}"
elif [ "${VERSION}" = "latest" ]; then
    base="https://github.com/${REPO}/releases/latest/download"
else
    base="https://github.com/${REPO}/releases/download/${VERSION}"
fi

work=$(mktemp -d)
staged=""
cleanup() { rm -rf "${work}"; [ -n "${staged}" ] && rm -f "${staged}"; return 0; }
trap cleanup EXIT INT TERM

say "Downloading drop for ${target}..."
case "${base}" in
    https://*) transport="--proto =https --tlsv1.2" ;;
    *) transport="" ;;
esac

# shellcheck disable=SC2086
curl -fsSL ${transport} -o "${work}/${archive}" "${base}/${archive}" ||
    die "could not download ${base}/${archive}"
# shellcheck disable=SC2086
curl -fsSL ${transport} -o "${work}/checksums.txt" "${base}/checksums.txt" ||
    die "could not download the checksums file"

( cd "${work}" && verify_checksum "${archive}" "checksums.txt" )
say "Checksum verified."

tar -xzf "${work}/${archive}" -C "${work}"
[ -f "${work}/drop" ] || die "the downloaded archive did not contain a drop binary"

mkdir -p "${INSTALL_DIR}"

# Stage inside the install directory so the final step is a same-filesystem
# rename. A plain `mv` from the temporary directory usually crosses a
# filesystem boundary and degrades to a copy, which is neither atomic nor safe
# against a copy of drop that is currently running.
staged="${INSTALL_DIR}/.drop.$$.new"
cp "${work}/drop" "${staged}"
chmod +x "${staged}"
mv -f "${staged}" "${INSTALL_DIR}/drop"
staged=""

say ""
say "Installed drop to ${INSTALL_DIR}/drop"

case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        say ""
        say "${INSTALL_DIR} is not on your PATH. Add it with:"
        say ""
        say "    export PATH=\"${INSTALL_DIR}:\$PATH\""
        ;;
esac

say ""
say "Send a file or folder:   drop send ./some-folder"
say "Receive it elsewhere:    drop recv <CODE>"
