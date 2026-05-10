# syntax=docker/dockerfile:1.7

ARG DEBIAN_VERSION=bookworm
ARG BUILDER_IMAGE=builder

FROM ${BUILDER_IMAGE} AS builder
FROM debian:${DEBIAN_VERSION}-slim AS runtime

LABEL org.opencontainers.image.title="evp"
LABEL org.opencontainers.image.description="Record terminal sessions from VHS-style scripts."
LABEL org.opencontainers.image.source="https://github.com/HalFrgrd/evp"
LABEL org.opencontainers.image.licenses="MIT"

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/evp /usr/local/bin/evp

WORKDIR /work
ENTRYPOINT ["/usr/local/bin/evp"]
