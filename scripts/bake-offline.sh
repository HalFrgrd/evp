#!/usr/bin/env bash
# scripts/bake-offline.sh — invoke `docker buildx bake` against the
# locally-tagged `evp-build-env:local` image instead of rebuilding the
# Rust+Zig toolchain stage from scratch.
#
# Use this from any environment that cannot reach `ziglang.org` or the
# upstream Ghostty / Zig package mirrors — most notably the Copilot
# cloud agent's restricted firewall. The companion
# `.github/workflows/copilot-setup-steps.yml` pulls
# `ghcr.io/<owner>/evp-build-env:latest` and tags it as
# `evp-build-env:local` while the firewall is still open, so by the
# time the agent runs `bake` the image (and its baked-in
# `/opt/ghostty-src` + `/opt/zig-global-cache`) is already on disk.
#
# Usage:
#   scripts/bake-offline.sh test
#   scripts/bake-offline.sh extract-binary stress_test
#   IMAGE=ghcr.io/halfrgrd/evp-build-env:latest scripts/bake-offline.sh test
#
# The two `--set` overrides below are the canonical ones documented in
# AGENTS.md ("Docker build environment image" section); keep changes
# in sync there.
set -euo pipefail

IMAGE="${IMAGE:-evp-build-env:local}"

if ! command -v docker >/dev/null 2>&1; then
    echo "scripts/bake-offline.sh: docker not found on PATH" >&2
    exit 127
fi

# Sanity-check the image is present locally. We deliberately do NOT
# try to `docker pull` here — pulling from inside a restricted
# environment is what we are working around.
if ! docker image inspect "${IMAGE}" >/dev/null 2>&1; then
    cat >&2 <<EOF
scripts/bake-offline.sh: image '${IMAGE}' not found locally.

Pull it once from an environment with internet access (or run the
'copilot-setup-steps' workflow), then retag it:

    docker pull ghcr.io/<owner>/evp-build-env:latest
    docker tag  ghcr.io/<owner>/evp-build-env:latest evp-build-env:local

Or invoke this script with IMAGE=<full ref> to use a different tag.
EOF
    exit 1
fi

exec docker buildx bake \
    --set "builder.contexts.build-env=docker-image://${IMAGE}" \
    --set "builder.args.BUILD_ENV_IMAGE=${IMAGE}" \
    "$@"
