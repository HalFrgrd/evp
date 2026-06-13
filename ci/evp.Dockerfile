# syntax=docker/dockerfile:1.7

ARG DEBIAN_VERSION=bookworm
ARG RUST_VERSION=1.95.0

FROM rust:${RUST_VERSION}-slim-${DEBIAN_VERSION} AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
        musl-tools \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Add x86_64 musl target
RUN rustup target add x86_64-unknown-linux-musl

ARG VERGEN_GIT_SHA
ARG VERGEN_GIT_BRANCH
ARG VERGEN_GIT_COMMIT_DATE
ARG VERGEN_GIT_DIRTY

ENV VERGEN_GIT_SHA=${VERGEN_GIT_SHA}
ENV VERGEN_GIT_BRANCH=${VERGEN_GIT_BRANCH}
ENV VERGEN_GIT_COMMIT_DATE=${VERGEN_GIT_COMMIT_DATE}
ENV VERGEN_GIT_DIRTY=${VERGEN_GIT_DIRTY}

WORKDIR /app

# Copy configuration, assets, and source files
COPY .cargo/ /app/.cargo/
COPY assets/ /app/assets/
COPY examples/ /app/examples/
COPY src/ /app/src/
COPY Cargo.toml Cargo.lock build.rs /app/

# Build release binary (using cache mounts for cargo registries and build directories to speed up compiles)
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    set -eux; \
    cargo build --release --target x86_64-unknown-linux-musl && \
    mkdir -p /out && \
    cp target/x86_64-unknown-linux-musl/release/evp /out/evp

FROM scratch AS export
COPY --from=builder /out/evp /
