# syntax=docker/dockerfile:1.7

ARG DEBIAN_VERSION=bookworm
ARG ZIG_VERSION=0.15.2
ARG GHOSTTY_COMMIT=6590196661f769dd8f2b3e85d6c98262c4ec5b3b

FROM --platform=$BUILDPLATFORM debian:${DEBIAN_VERSION} AS build-libghostty
ARG ZIG_VERSION
ARG GHOSTTY_COMMIT
ARG BUILDARCH
ARG TARGETARCH

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        curl \
        git \
        pkg-config \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    case "$BUILDARCH" in \
        amd64) zig_arch="x86_64" ;; \
        arm64) zig_arch="aarch64" ;; \
        *) echo "unsupported build arch $BUILDARCH" >&2; exit 1 ;; \
    esac; \
    tarball="zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz"; \
    curl -fsSLo "/tmp/${tarball}" "https://ziglang.org/download/${ZIG_VERSION}/${tarball}"; \
    mkdir -p /opt; \
    tar -xJf "/tmp/${tarball}" -C /opt; \
    mv "/opt/zig-${zig_arch}-linux-${ZIG_VERSION}" /opt/zig; \
    rm "/tmp/${tarball}"

ENV PATH="/opt/zig:${PATH}"

# Clone ghostty at the pinned commit and build libghostty-vt directly via zig.
# -Dtarget=<arch>-linux-musl: build against musl libc so the .a is compatible
#   with both glibc and musl linkers.  glibc has all musl symbols; the reverse
#   is not true (e.g. mmap64 is glibc-only and breaks musl static linking).
# -Dcpu=baseline: target the generic CPU baseline so the cached artefact is
#   portable across different CI runner microarchitectures.
RUN git clone --filter=blob:none --no-checkout \
        https://github.com/ghostty-org/ghostty.git /ghostty && \
    git -C /ghostty checkout "${GHOSTTY_COMMIT}"

WORKDIR /ghostty
RUN --mount=type=cache,target=/zig-cache \
        set -eux; \
        case "$TARGETARCH" in \
            amd64) zig_target="x86_64-linux-musl" ;; \
            arm64) zig_target="aarch64-linux-musl" ;; \
            *) echo "unsupported target arch $TARGETARCH" >&2; exit 1 ;; \
        esac; \
        zig build \
            -Demit-lib-vt \
            -Doptimize=ReleaseFast \
            -Demit-xcframework=false \
            -Dapp-runtime=none \
            -Dtarget="${zig_target}" \
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
