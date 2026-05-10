# syntax=docker/dockerfile:1.7

ARG BUILDER_IMAGE=builder
FROM ${BUILDER_IMAGE} AS builder
FROM scratch AS binary
COPY --from=builder /usr/local/bin/evp /evp
