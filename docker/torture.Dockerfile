# syntax=docker/dockerfile:1.7

ARG BUILDER_IMAGE=builder
FROM ${BUILDER_IMAGE} AS torture

RUN apt-get update && apt-get install -y --no-install-recommends \
        bash \
        coreutils \
        gawk \
        python3 \
    && rm -rf /var/lib/apt/lists/*

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --example torture_benchmark \
 && cp /src/target/release/examples/torture_benchmark /usr/local/bin/torture_benchmark

ENTRYPOINT []
WORKDIR /src
