# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.95
ARG DEBIAN_VERSION=bookworm
ARG ZIG_VERSION=0.15.2

FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS build-env
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
