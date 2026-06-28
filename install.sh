#!/bin/sh
# evp installer — downloads a prebuilt evp binary from the latest GitHub
# release.
#
# Usage:
#   curl -sSfL https://raw.githubusercontent.com/HalFrgrd/evp/master/install.sh | sh
#
# Override the version (default `latest`):
#   curl -sSfL .../install.sh | EVP_VERSION=v0.2.0 sh
#
# Override the default install dir:
#   curl -sSfL .../install.sh | EVP_INSTALL_DIR=~/.local/bin sh

set -eu

REPO="HalFrgrd/evp"
INSTALL_DIR="${EVP_INSTALL_DIR:-$PWD}"
VERSION_OVERRIDE="${EVP_VERSION:-}"

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

get_latest_version() {
    url="https://github.com/${REPO}/releases/latest"
    if command -v curl >/dev/null 2>&1; then
        tag_url="$(curl -sI "$url" | grep -i '^location:' | head -1)"
    elif command -v wget >/dev/null 2>&1; then
        tag_url="$(wget --max-redirect=0 --server-response -O /dev/null "$url" 2>&1 | grep -i 'location:' | head -1)"
    else
        err "Neither curl nor wget is available. Please install one and retry."
    fi
    version="$(printf '%s' "$tag_url" | sed 's|.*/||' | cut -d' ' -f1 | tr -d '\r\n')"
    [ -n "$version" ] || err "Could not determine latest version from GitHub Release redirect."
    echo "$version"
}

verify_sha256() {
    sha256_file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c "$sha256_file"
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 -c "$sha256_file"
    else
        err "No checksum tool found (sha256sum or shasum). Cannot verify download."
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

if [ -n "$VERSION_OVERRIDE" ]; then
    TAG="$VERSION_OVERRIDE"
    say "Using specified release version: ${TAG}"
else
    say "Fetching latest release info from github.com/${REPO}..."
    TAG="$(get_latest_version)"
    say "Latest version: ${TAG}"
fi

VERSION_NO_V="${TAG#v}"
STAGE="evp-${VERSION_NO_V}-${TARGET}"
ARCHIVE="${STAGE}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${ARCHIVE}"
SHA_URL="${URL}.sha256"



# --- Download + verify -----------------------------------------------------

TMPDIR="$(mktemp -d)"
# shellcheck disable=SC2064
trap "rm -rf '$TMPDIR'" EXIT

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

    ${RUN_CMD} run-sample-script
    # → writes ./evp-test.gif (a small built-in demo)

Or record your own terminal session:

    ${RUN_CMD} record --output demo.tape

EOF
