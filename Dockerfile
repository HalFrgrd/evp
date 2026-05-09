# syntax=docker/dockerfile:1.7
#
# Multi-stage build for the evp binary.
#
# Both `libghostty-vt` and `libghostty-vt-sys` are pulled from crates.io,
# so this Dockerfile is fully self-contained — no git clone of a sibling
# checkout, no path deps. Building still requires Zig 0.15.x because the
# sys crate's build.rs invokes `zig build` to compile the upstream
# Ghostty source.
#
# - The builder stage installs Rust + Zig 0.15.2 and produces a release
#   `evp` binary with `libghostty-vt.a` statically linked in.
# - The runtime stage is a slim Debian image carrying only the binary,
#   `/bin/sh`, ca-certificates, and a free monospace TTF so evp's GIF
#   renderer always has a fallback font.

ARG RUST_VERSION=1.95
ARG DEBIAN_VERSION=bookworm

# ---------------------------------------------------------------------------
# Builder
# ---------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS builder

ARG ZIG_VERSION=0.15.2
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

# Build deps for libghostty-vt-sys's vendored Zig build:
#   - xz-utils  : extract the Zig tarball
#   - clang / pkg-config : compiling sys crates
#   - curl / ca-certificates : download Zig
#   - git       : libghostty-vt-sys's build.rs fetches the pinned Ghostty
#                 source at build time when GHOSTTY_SOURCE_DIR is unset.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        curl \
        git \
        pkg-config \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

# Install Zig 0.15.x. Architecture is detected at build time so the same
# Dockerfile works for amd64 and arm64.
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

# Build a release binary. Cache cargo's registry between builds for
# faster CI iteration.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release \
 && cp /src/target/release/evp /usr/local/bin/evp

# ---------------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------------
FROM debian:${DEBIAN_VERSION}-slim AS runtime

LABEL org.opencontainers.image.title="evp"
LABEL org.opencontainers.image.description="Record terminal sessions from VHS-style scripts."
LABEL org.opencontainers.image.source="https://github.com/HalFrgrd/evp"
LABEL org.opencontainers.image.licenses="MIT"

# /bin/sh is in coreutils. fonts-dejavu-core gives us DejaVuSansMono.ttf
# which evp's renderer auto-discovers via fontdb when --font is omitted.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        fonts-dejavu-core \
        libfontconfig1 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/evp /usr/local/bin/evp

WORKDIR /work
ENTRYPOINT ["/usr/local/bin/evp"]
