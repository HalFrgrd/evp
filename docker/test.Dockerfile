# syntax=docker/dockerfile:1.7

ARG BUILDER_IMAGE=builder
FROM ${BUILDER_IMAGE} AS test

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --workspace \
 && cargo test --workspace --lib --bins -- --test-threads=1 \
 && cargo test --workspace --tests -- --test-threads=1
