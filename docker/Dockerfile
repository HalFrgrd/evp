# syntax=docker/dockerfile:1.7
#
# Multi-stage build for the evp binary.
#
# Both `libghostty-vt` and `libghostty-vt-sys` are pulled from crates.io,
# so this Dockerfile is fully self-contained — no git clone of a sibling
# checkout, no path deps. Building requires Zig 0.15.x because the sys
# crate's build.rs invokes `zig build` to compile the upstream Ghostty
# source.
#
# Static musl build:
#   We link against musl libc with `+crt-static` (the default for the
#   `*-unknown-linux-musl` targets), producing a fully static `evp`
#   binary that runs on any Linux kernel — alpine, distroless, scratch,
#   ancient RHEL, embedded — without any glibc or shared-library
#   compatibility juggling. mimalloc is wired in as the global
#   allocator (see src/bin/evp.rs) to recover the perf gap musl's
#   default allocator otherwise leaves on alloc-heavy workloads.
#
# Stages:
#   builder — Rust + Zig 0.15.x; produces a static `evp` linked against
#             musl with libghostty-vt.a bundled in.
#   test    — runs `cargo build/test --workspace`. Used by ci.yml.
#   runtime — Debian slim image with /bin/sh, ca-certs, and DejaVu Mono
#             as a renderer fallback font. The binary itself doesn't
#             need any of these, but the image is convenient for
#             `docker run` users who want a posix shell available
#             alongside evp.

ARG RUST_VERSION=1.95
ARG DEBIAN_VERSION=bookworm
# Static musl target. No glibc version juggling — the resulting binary
# carries its own libc and runs on any Linux kernel from the last
# decade or so.
ARG TARGET=x86_64-unknown-linux-musl

# ---------------------------------------------------------------------------
# Builder
# ---------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS builder

ARG ZIG_VERSION=0.15.2
ARG TARGET
ARG VERGEN_GIT_SHA=unknown
ARG VERGEN_GIT_BRANCH=unknown
ARG VERGEN_GIT_COMMIT_DATE=unknown
ARG VERGEN_GIT_COMMIT_TIMESTAMP=unknown
ARG VERGEN_GIT_COMMIT_COUNT=unknown
ARG VERGEN_GIT_COMMIT_AUTHOR_NAME=unknown
ARG VERGEN_GIT_COMMIT_AUTHOR_EMAIL=unknown
ARG VERGEN_GIT_COMMIT_MESSAGE=unknown
ARG VERGEN_GIT_DESCRIBE=unknown
ARG VERGEN_GIT_DIRTY=unknown

# Build deps:
#   - musl-tools / musl-dev  : musl-gcc + headers (used by cc-rs for
#                              C bits like mimalloc's runtime)
#   - clang / pkg-config     : misc -sys crates
#   - curl / xz-utils / ca-certs : download + extract Zig
#   - git                    : libghostty-vt-sys's build.rs fetches the
#                              pinned Ghostty source at build time when
#                              GHOSTTY_SOURCE_DIR is unset.
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

# Install Zig 0.15.x. Architecture is detected at build time so the
# same Dockerfile works for amd64 and arm64.
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

RUN rustup target add ${TARGET}

# Build scripts that compile native code (cc-rs) need both C and C++
# compilers for the musl target. We route both through Zig so crates
# like woofwoof (C++) don't look for x86_64-linux-musl-g++.
# We deliberately do NOT override the Rust linker: rustc's default for
# the musl target uses bundled musl + rust-lld with `+crt-static` and
# `link-self-contained`, producing a fully static binary.
ENV CC_x86_64_unknown_linux_musl="zig cc -target x86_64-linux-musl" \
    CXX_x86_64_unknown_linux_musl="zig c++ -target x86_64-linux-musl" \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=yes -C linker=rust-lld"

ENV VERGEN_GIT_SHA="${VERGEN_GIT_SHA}" \
    VERGEN_GIT_BRANCH="${VERGEN_GIT_BRANCH}" \
    VERGEN_GIT_COMMIT_DATE="${VERGEN_GIT_COMMIT_DATE}" \
    VERGEN_GIT_COMMIT_TIMESTAMP="${VERGEN_GIT_COMMIT_TIMESTAMP}" \
    VERGEN_GIT_COMMIT_COUNT="${VERGEN_GIT_COMMIT_COUNT}" \
    VERGEN_GIT_COMMIT_AUTHOR_NAME="${VERGEN_GIT_COMMIT_AUTHOR_NAME}" \
    VERGEN_GIT_COMMIT_AUTHOR_EMAIL="${VERGEN_GIT_COMMIT_AUTHOR_EMAIL}" \
    VERGEN_GIT_COMMIT_MESSAGE="${VERGEN_GIT_COMMIT_MESSAGE}" \
    VERGEN_GIT_DESCRIBE="${VERGEN_GIT_DESCRIBE}" \
    VERGEN_GIT_DIRTY="${VERGEN_GIT_DIRTY}"

WORKDIR /src
COPY . /src

# Build a fully static release binary. The /src/target cache mount is
# only live during this RUN, so we `cp` the binary out before the
# mount unmounts.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --bin evp --target ${TARGET} \
 && cp /src/target/${TARGET}/release/evp /usr/local/bin/evp \
 && /usr/local/bin/evp --version | head -n3 \
 && echo "--- file ---" && file /usr/local/bin/evp \
 && echo "--- ldd (should say 'not a dynamic executable' / 'statically linked') ---" \
 && (ldd /usr/local/bin/evp 2>&1 || true)

# ---------------------------------------------------------------------------
# Test
# ---------------------------------------------------------------------------
# Runs the workspace test suite using the same toolchain as the builder.
# Used by ci.yml via `docker build --target test`. Tests build for the
# host's default target (glibc) for speed — they don't have to be
# portable, just correct.
FROM builder AS test

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --workspace \
 && cargo test --workspace -- --test-threads=1

# ---------------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------------
FROM debian:${DEBIAN_VERSION}-slim AS runtime

LABEL org.opencontainers.image.title="evp"
LABEL org.opencontainers.image.description="Record terminal sessions from VHS-style scripts."
LABEL org.opencontainers.image.source="https://github.com/HalFrgrd/evp"
LABEL org.opencontainers.image.licenses="MIT"

# The static musl evp doesn't need any of these, but they make the
# image friendlier for `docker run` users (a posix shell, ca-certs for
# https-based tape scripts, a system mono font for `--font` callers).
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/evp /usr/local/bin/evp

WORKDIR /work
ENTRYPOINT ["/usr/local/bin/evp"]

# ---------------------------------------------------------------------------
# Binary extraction (for `docker buildx bake extract-binary`)
# ---------------------------------------------------------------------------
# Empty scratch image whose entire filesystem is just the static evp
# binary. Used by `docker-bake.hcl`'s `extract-binary` target with
# `output = "type=local,dest=docker/build"` so the buildx output writer
# drops the binary straight onto the host filesystem — no `docker
# create` / `docker cp` dance required.
FROM scratch AS binary
COPY --from=builder /usr/local/bin/evp /evp
