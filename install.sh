#!/bin/sh
# evp installer — downloads a prebuilt evp binary from the latest GitHub
# release into the current working directory.
#
# Usage:
#   curl -sSfL https://raw.githubusercontent.com/HalFrgrd/evp/master/install.sh | sh
#
# Override the version (default `latest`):
#   curl -sSfL .../install.sh | EVP_VERSION=v0.2.0 sh
#
# Override the install dir (default $PWD):
#   curl -sSfL .../install.sh | EVP_INSTALL_DIR=~/.local/bin sh

set -eu

REPO="HalFrgrd/evp"
INSTALL_DIR="${EVP_INSTALL_DIR:-$PWD}"
VERSION="${EVP_VERSION:-latest}"

say()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
err()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

download() {
    url="$1"; dest="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$dest" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$dest" "$url"
    else
        err "Neither curl nor wget is available."
    fi
}

fetch_text() {
    url="$1"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- "$url"
    else
        err "Neither curl nor wget is available."
    fi
}

verify_sha256() {
    file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c "$file"
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 -c "$file"
    else
        warn "No checksum tool (sha256sum/shasum) — skipping verification."
    fi
}

# --- Platform detection ----------------------------------------------------

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux) os_tag="unknown-linux-musl" ;;
    *)
        err "Unsupported OS: $OS. Only Linux prebuilt binaries are published today.
Build from source instead: https://github.com/${REPO}#build-from-source"
        ;;
esac

case "$ARCH" in
    x86_64|amd64) arch_tag="x86_64" ;;
    *)
        err "Unsupported architecture: $ARCH. Only x86_64 prebuilt binaries are published today.
Build from source instead: https://github.com/${REPO}#build-from-source"
        ;;
esac

TARGET="${arch_tag}-${os_tag}"
say "Detected target: ${TARGET}"

# --- Resolve release tag ---------------------------------------------------

if [ "$VERSION" = "latest" ]; then
    say "Fetching latest release info from github.com/${REPO}..."
    api_url="https://api.github.com/repos/${REPO}/releases/latest"
else
    api_url="https://api.github.com/repos/${REPO}/releases/tags/${VERSION}"
fi

release_json="$(fetch_text "$api_url")"
TAG="$(printf '%s' "$release_json" | grep -m1 '"tag_name"' \
    | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')"
[ -n "$TAG" ] || err "Could not resolve release tag from ${api_url}."

VERSION_NO_V="${TAG#v}"
STAGE="evp-${VERSION_NO_V}-${TARGET}"
ARCHIVE="${STAGE}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${ARCHIVE}"
SHA_URL="${URL}.sha256"

say "Resolved release: ${TAG}"

# --- Download + verify -----------------------------------------------------

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

say "Downloading ${ARCHIVE}..."
download "$URL" "${TMPDIR}/${ARCHIVE}"

if download "$SHA_URL" "${TMPDIR}/${ARCHIVE}.sha256" 2>/dev/null; then
    say "Verifying checksum..."
    (cd "$TMPDIR" && verify_sha256 "${ARCHIVE}.sha256") \
        || err "Checksum verification failed."
else
    warn "No .sha256 published for ${TAG}; skipping verification."
fi

# --- Extract + install -----------------------------------------------------

tar -xzf "${TMPDIR}/${ARCHIVE}" -C "${TMPDIR}"
mkdir -p "$INSTALL_DIR"
install -m 755 "${TMPDIR}/${STAGE}/evp" "${INSTALL_DIR}/evp"

ABS_INSTALL_DIR=$(cd "$INSTALL_DIR" && pwd)
ABS_PWD=$(pwd)

if [ "$ABS_INSTALL_DIR" = "$ABS_PWD" ]; then
    RUN_CMD="./evp"
else
    RUN_CMD="${INSTALL_DIR}/evp"
fi

say "Installed: ${INSTALL_DIR}/evp"
"${INSTALL_DIR}/evp" --version | head -n1

cat <<EOF

Try it out:

    ${RUN_CMD} run-test-script
    # → writes ./evp-test.gif (a small built-in demo)

Or render your own .tape script:

    ${RUN_CMD} my_script.tape --output demo.gif --output demo.svg

EOF
