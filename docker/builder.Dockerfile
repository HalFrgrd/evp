# syntax=docker/dockerfile:1.7

ARG TARGET=x86_64-unknown-linux-musl
ARG BUILD_ENV_IMAGE=build-env
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

FROM ${BUILD_ENV_IMAGE} AS builder

ARG TARGET
ARG VERGEN_GIT_SHA
ARG VERGEN_GIT_BRANCH
ARG VERGEN_GIT_COMMIT_DATE
ARG VERGEN_GIT_COMMIT_TIMESTAMP
ARG VERGEN_GIT_COMMIT_COUNT
ARG VERGEN_GIT_COMMIT_AUTHOR_NAME
ARG VERGEN_GIT_COMMIT_AUTHOR_EMAIL
ARG VERGEN_GIT_COMMIT_MESSAGE
ARG VERGEN_GIT_DESCRIBE
ARG VERGEN_GIT_DIRTY

RUN rustup target add ${TARGET}

ENV CC_x86_64_unknown_linux_musl=musl-gcc \
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
COPY --from=libghostty / /src/assets/libghostty

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --bin evp --target ${TARGET} \
 && cp /src/target/${TARGET}/release/evp /usr/local/bin/evp \
 && /usr/local/bin/evp --version | head -n3 \
 && echo "--- file ---" && file /usr/local/bin/evp \
 && echo "--- ldd (should say 'not a dynamic executable' / 'statically linked') ---" \
 && (ldd /usr/local/bin/evp 2>&1 || true)
