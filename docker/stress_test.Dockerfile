# syntax=docker/dockerfile:1.7

ARG BUILDER_IMAGE=builder
ARG VHS_IMAGE=ghcr.io/charmbracelet/vhs:v0.10.0
ARG STRESS_TEST_FPS=60

FROM ${BUILDER_IMAGE} AS stress_test_build
RUN apt-get update && apt-get install -y --no-install-recommends \
        bash \
        coreutils \
        gawk \
        python3 \
    && rm -rf /var/lib/apt/lists/*

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --example stress_test_benchmark \
 && cp /src/target/release/examples/stress_test_benchmark /usr/local/bin/stress_test_benchmark \
 && install -D -m 0644 /src/examples/stress_test.tape /opt/stress_test/stress_test.tape \
 && install -D -m 0755 /src/scripts/stress_test_compare.sh /opt/stress_test/stress_test_compare.sh \
 && install -D -m 0755 /src/scripts/stress_test_program.py /opt/stress_test/stress_test_program.py \
 && install -D -m 0755 /src/scripts/gif_frame_analyzer.py /opt/stress_test/gif_frame_analyzer.py

FROM ${VHS_IMAGE} AS stress_test_runner

ARG VHS_IMAGE
ARG STRESS_TEST_FPS
ENV STRESS_TEST_FPS=${STRESS_TEST_FPS} \
    HOME=/tmp \
    XDG_CONFIG_HOME=/tmp/chrome-config \
    XDG_CACHE_HOME=/tmp/chrome-cache \
    XDG_RUNTIME_DIR=/tmp/chrome-runtime

COPY --from=stress_test_build /usr/local/bin/stress_test_benchmark /usr/local/bin/stress_test_benchmark
COPY --from=stress_test_build /opt/stress_test /opt/stress_test

WORKDIR /work
RUN set -eux; \
    mkdir -p /out /tmp; \
    install -D -m 0755 /opt/stress_test/stress_test_program.py /tmp/stress_test_program.py; \
    EVP_STRESS_TEST_TAPE=/opt/stress_test/stress_test.tape \
    EVP_STRESS_TEST_PROGRAM=/opt/stress_test/stress_test_program.py \
      /usr/local/bin/stress_test_benchmark /out/evp.gif /out/evp.report.txt; \
    start_ns=$(date +%s%N); \
    vhs /opt/stress_test/stress_test.tape; \
    end_ns=$(date +%s%N); \
    wall_ms=$(( (end_ns - start_ns) / 1000000 )); \
    test -s /work/stress_test.gif; \
    mv /work/stress_test.gif /out/vhs.gif; \
    vhs_cpu=$(awk '/Cpus_allowed_list/ {print $2}' /proc/self/status); \
    { \
      echo "=== vhs stress_test benchmark ==="; \
      echo "renderer            = vhs"; \
      echo "image               = ${VHS_IMAGE}"; \
      echo "output_gif          = /out/vhs.gif"; \
      echo "output_gif_bytes    = $(stat -c %s /out/vhs.gif)"; \
      echo "wall_ms             = ${wall_ms}"; \
      echo "cpu_affinity        = ${vhs_cpu:-(unknown)}"; \
    } > /out/vhs.report.txt; \
    /opt/stress_test/stress_test_compare.sh \
      /out/evp.gif \
      /out/evp.report.txt \
      /out/vhs.gif \
      /out/vhs.report.txt \
      /out/comparison.md; \
    python3 /opt/stress_test/gif_frame_analyzer.py /out/evp.gif \
      --fps "${STRESS_TEST_FPS}" --json > /out/evp.gif.analysis.json; \
    python3 /opt/stress_test/gif_frame_analyzer.py /out/vhs.gif \
      --fps "${STRESS_TEST_FPS}" --json > /out/vhs.gif.analysis.json

FROM scratch AS stress_test
COPY --from=stress_test_runner /out/ /
