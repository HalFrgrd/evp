# syntax=docker/dockerfile:1.7
#
# Multi-stage build for the evp binary.
#
# - The builder stage installs a Rust toolchain, downloads the Zig 0.15.2
#   pre-built tarball that libghostty's `build.zig` requires, clones the
#   sibling `libghostty-rs` repository at the same path our Cargo
#   workspace expects (`../libghostty-rs/...`), and produces a fully
#   statically linked `evp` binary (against `libghostty-vt.a`).
# - The runtime stage is a slim Debian image carrying only the binary,
#   `/bin/sh`, ca-certificates, and a free monospace TTF so that the GIF
#   renderer always has a fallback font.
#
# Pinning the libghostty-rs commit keeps the image reproducible. Bump the
# `LIBGHOSTTY_RS_REF` ARG to upgrade.

ARG RUST_VERSION=1.83
ARG DEBIAN_VERSION=bookworm

# ---------------------------------------------------------------------------
# Builder
# ---------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS builder

ARG ZIG_VERSION=0.15.2
ARG LIBGHOSTTY_RS_REPO=https://github.com/Uzaaft/libghostty-rs.git
ARG LIBGHOSTTY_RS_REF=5ac47e9eb166add2c00c432bc65c279133629712

# Build deps for libghostty-vt-sys (xz to extract Zig, git to clone
# libghostty-rs, pkg-config / clang for sys crates).
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        curl \
        git \
        pkg-config \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

# Install Zig 0.15.x. We pick the architecture at build time so the same
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

# Lay out the sibling libghostty-rs checkout the way our path dep
# expects.
WORKDIR /src
RUN git clone "${LIBGHOSTTY_RS_REPO}" libghostty-rs \
    && git -C libghostty-rs checkout "${LIBGHOSTTY_RS_REF}"

# Copy the evp source tree. Use a .dockerignore to keep this small.
COPY . /src/evp
WORKDIR /src/evp

# Build a release binary with the static link feature. Cache cargo's
# registry between builds for faster CI iteration.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/evp/target \
    cargo build --release \
 && cp /src/evp/target/release/evp /usr/local/bin/evp

# ---------------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------------
FROM debian:${DEBIAN_VERSION}-slim AS runtime

LABEL org.opencontainers.image.title="evp"
LABEL org.opencontainers.image.description="Record terminal sessions from VHS-style scripts."
LABEL org.opencontainers.image.source="https://github.com/Uzaaft/evp"
LABEL org.opencontainers.image.licenses="MIT"

# /bin/sh is in coreutils. fonts-dejavu-core gives us DejaVuSansMono.ttf
# which evp's renderer auto-discovers via fontdb when --font is omitted.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        fonts-dejavu-core \
        libfontconfig1 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/evp /usr/local/bin/evp

# Default working directory is bind-mounted by the user. Running
# `docker run --rm -v $PWD:/work ghcr.io/uzaaft/evp examples/hello.tape`
# Just Works.
WORKDIR /work
ENTRYPOINT ["/usr/local/bin/evp"]
