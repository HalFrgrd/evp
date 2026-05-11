# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.95
ARG DEBIAN_VERSION=bookworm
ARG ZIG_VERSION=0.15.2

# ----------------------------------------------------------------------------
# Stage 1: build-env-base — Rust + Zig toolchain.
#
# This is the original `build-env` image. It is kept as its own stage so the
# Zig install layer stays cacheable independently of the (much larger)
# pre-warmed Ghostty / Zig package cache produced in stage 2.
# ----------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS build-env-base
ARG ZIG_VERSION

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        curl \
        file \
        git \
        musl-dev \
        musl-tools \
        pkg-config \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    arch="$(uname -m)"; \
    case "$arch" in \
        x86_64)  zig_arch="x86_64" ;; \
        aarch64) zig_arch="aarch64" ;; \
        *) echo "unsupported arch $arch" >&2; exit 1 ;; \
    esac; \
    tarball="zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz"; \
    curl -fsSLo "/tmp/${tarball}" "https://ziglang.org/download/${ZIG_VERSION}/${tarball}"; \
    mkdir -p /opt; \
    tar -xJf "/tmp/${tarball}" -C /opt; \
    mv "/opt/zig-${zig_arch}-linux-${ZIG_VERSION}" /opt/zig; \
    rm "/tmp/${tarball}"

ENV PATH="/opt/zig:${PATH}"

# ----------------------------------------------------------------------------
# Stage 2: build-env (default) — base toolchain + warmed Ghostty/Zig cache.
#
# The `docker/cache-warmer` fixture crate is built here once. That:
#   * clones the upstream Ghostty source pinned inside libghostty-vt-sys's
#     build.rs into the cargo OUT_DIR, and
#   * lets `zig build` resolve every Zig package dependency into the shared
#     `ZIG_GLOBAL_CACHE_DIR` (Zig honours that env var natively).
#
# We then promote both to fixed paths inside the image — `/opt/ghostty-src`
# and `/opt/zig-global-cache` — and export the env vars that
# `libghostty-vt-sys` (`GHOSTTY_SOURCE_DIR`) and `zig` (`ZIG_GLOBAL_CACHE_DIR`)
# read at build time. Downstream stages (`docker/builder.Dockerfile` and any
# environment that pulls this image, including the Copilot cloud agent) get
# a fully offline Ghostty + Zig package cache for free.
#
# IMPORTANT: the `libghostty-vt` git rev pinned in
# `docker/cache-warmer/Cargo.toml` MUST match the rev in the workspace-root
# `Cargo.toml`. Bumping one without the other will silently leave the cache
# stale and force a network fetch in the builder. The `build-env` workflow
# triggers on changes to both files for exactly this reason.
# ----------------------------------------------------------------------------
FROM build-env-base AS build-env

ENV GHOSTTY_SOURCE_DIR=/opt/ghostty-src \
    ZIG_GLOBAL_CACHE_DIR=/opt/zig-global-cache

# Copy only the warmer crate (not the rest of the repo) so this layer
# invalidates only when the warmer's Cargo.toml / Cargo.lock change.
WORKDIR /tmp/cache-warmer
COPY docker/cache-warmer/Cargo.toml ./Cargo.toml
COPY docker/cache-warmer/src ./src

# Pre-fetch ghostty + every Zig package the build needs. We unset
# `GHOSTTY_SOURCE_DIR` for this single invocation because we want
# libghostty-vt-sys's build.rs to take its "fetch into OUT_DIR" branch
# (`fetch_ghostty`) — that's how we get a clean copy of the pinned ghostty
# source we can lift out of OUT_DIR. `ZIG_GLOBAL_CACHE_DIR` stays set so
# zig deposits all package downloads where we want them.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    set -eux; \
    mkdir -p "${ZIG_GLOBAL_CACHE_DIR}"; \
    env -u GHOSTTY_SOURCE_DIR cargo build --release; \
    out_dir="$(find target/release/build -maxdepth 1 -type d -name 'libghostty-vt-sys-*' \
        -exec test -d '{}/out/ghostty-src' \; -print | head -n1)/out"; \
    test -n "${out_dir}" -a -d "${out_dir}/ghostty-src"; \
    cp -a "${out_dir}/ghostty-src" "${GHOSTTY_SOURCE_DIR}"; \
    test -f "${GHOSTTY_SOURCE_DIR}/build.zig"; \
    # Drop the warmer build artefacts; we only wanted the side-effect caches.
    rm -rf /tmp/cache-warmer

WORKDIR /
