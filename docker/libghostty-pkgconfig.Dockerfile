# syntax=docker/dockerfile:1.7

ARG DEBIAN_VERSION=bookworm
ARG ZIG_VERSION=0.15.2
ARG GHOSTTY_COMMIT=6590196661f769dd8f2b3e85d6c98262c4ec5b3b

FROM debian:${DEBIAN_VERSION} AS build-libghostty
ARG ZIG_VERSION
ARG GHOSTTY_COMMIT

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        curl \
        git \
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

# Clone ghostty at the pinned commit and build libghostty-vt directly via zig.
# -Dcpu=baseline ensures the static library targets the generic architecture
# baseline rather than the native CPU of the build runner, making the cached
# artefact portable across different CI runner microarchitectures.
RUN git clone --filter=blob:none --no-checkout \
        https://github.com/ghostty-org/ghostty.git /ghostty && \
    git -C /ghostty checkout "${GHOSTTY_COMMIT}"

WORKDIR /ghostty
RUN --mount=type=cache,target=/zig-cache \
        set -eux; \
        zig build \
            -Demit-lib-vt \
            -Doptimize=ReleaseFast \
            -Demit-xcframework=false \
            -Dapp-runtime=none \
            -Dcpu=baseline \
            --prefix /ghostty-install \
            --cache-dir /zig-cache; \
        test -f /ghostty-install/lib/libghostty-vt.a; \
        test -f /ghostty-install/include/ghostty/vt.h; \
        test -f /ghostty-install/share/pkgconfig/libghostty-vt-static.pc; \
        rm -rf /out && mkdir -p /out/lib /out/include /out/share/pkgconfig; \
        cp -a /ghostty-install/include/ghostty /out/include/; \
        cp /ghostty-install/lib/libghostty-vt.a /out/lib/; \
        cp /ghostty-install/share/pkgconfig/libghostty-vt-static.pc /out/share/pkgconfig/; \
        sed -i 's|^prefix=.*|prefix=${pcfiledir}/../..|' /out/share/pkgconfig/libghostty-vt-static.pc

FROM scratch AS export
COPY --from=build-libghostty /out/ /
