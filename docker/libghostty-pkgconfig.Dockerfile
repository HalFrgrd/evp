# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.95
ARG DEBIAN_VERSION=bookworm
ARG ZIG_VERSION=0.15.2
ARG LIBGHOSTTY_RS_REV=5ac47e9eb166add2c00c432bc65c279133629712

FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS build-libghostty
ARG ZIG_VERSION
ARG LIBGHOSTTY_RS_REV

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

# Build libghostty-vt-sys in release mode and keep it architecture-generic by
# relying on the target baseline (never pass -Dcpu=native).
WORKDIR /seed
RUN mkdir -p /seed/src
RUN printf '%s\n' \
    '[package]' \
    'name = "libghostty-seed"' \
    'version = "0.1.0"' \
    'edition = "2021"' \
    '' \
    '[dependencies]' \
    "libghostty-vt-sys = { git = \"https://github.com/uzaaft/libghostty-rs\", rev = \"${LIBGHOSTTY_RS_REV}\", features = [\"link-static\", \"pkg-config\"] }" \
    > /seed/Cargo.toml
RUN printf '%s\n' \
    'extern crate libghostty_vt_sys;' \
    '' \
    'pub fn seed() {}' \
    > /seed/src/lib.rs

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/seed/target \
        set -eux; \
        CARGO_TARGET_DIR=/seed/target LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast cargo build --release; \
        out_dir="$(find /seed/target/release/build -maxdepth 2 -type d -path '*/libghostty-vt-sys-*/out' | head -n1)"; \
        test -n "$out_dir"; \
        install_root="$out_dir/ghostty-install"; \
        test -f "$install_root/lib/libghostty-vt.a"; \
        test -f "$install_root/include/ghostty/vt.h"; \
        test -f "$install_root/share/pkgconfig/libghostty-vt-static.pc"; \
        rm -rf /out && mkdir -p /out; \
        mkdir -p /out/lib /out/include /out/share/pkgconfig; \
        cp -a "$install_root/include/ghostty" /out/include/; \
        cp "$install_root/lib/libghostty-vt.a" /out/lib/; \
        cp "$install_root/share/pkgconfig/libghostty-vt-static.pc" /out/share/pkgconfig/; \
        sed -i 's|^prefix=.*|prefix=${pcfiledir}/../..|' /out/share/pkgconfig/libghostty-vt-static.pc

FROM scratch AS export
COPY --from=build-libghostty /out/ /
