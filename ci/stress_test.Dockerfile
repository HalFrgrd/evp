# syntax=docker/dockerfile:1.7
FROM ghcr.io/charmbracelet/vhs:v0.11.0

# Install dependencies (python3, procps for CPU affinity check if needed)
RUN apt-get update && apt-get install -y --no-install-recommends \
        python3 \
        procps \
    && rm -rf /var/lib/apt/lists/*

# Copy the compiled evp binary and evp_helper_tool binary from the build context
COPY --from=evp-binary /evp /usr/local/bin/evp
COPY --from=evp-binary /evp_helper_tool /usr/local/bin/evp_helper_tool

WORKDIR /work

# Copy the required scripts and tape files
COPY scripts/ /work/scripts/
COPY examples/ /work/examples/

# Run the stress tests, VHS rendering, comparison, and analysis
# We write the outputs directly to /out/
RUN set -euo pipefail; \
    mkdir -p /out; \
    # 1. Run evp directly to generate gif and stats
    evp /work/scripts/stress_test.tape --output /out/evp.gif --output /out/evp.stats; \
    # 2. Run VHS stress test (direct call to vhs!)
    start_ns=$(date +%s%N); \
    vhs /work/scripts/stress_test.tape -o /out/vhs.gif; \
    end_ns=$(date +%s%N); \
    wall_ms=$(( (end_ns - start_ns) / 1000000 )); \
    # Write vhs.report.txt
    { \
      echo "=== vhs stress_test benchmark ==="; \
      echo "renderer            = vhs"; \
      echo "image               = ghcr.io/charmbracelet/vhs:v0.11.0"; \
      echo "output_gif          = /out/vhs.gif"; \
      echo "output_gif_bytes    = $(stat -c %s /out/vhs.gif)"; \
      echo "wall_ms             = ${wall_ms}"; \
      echo "cpu_affinity        = (unknown)"; \
    } > /out/vhs.report.txt; \
    # 3. Run frame analysis and comparison
    /work/scripts/stress_test_compare.sh \
      /out/evp.gif \
      /out/evp.stats \
      /out/vhs.gif \
      /out/vhs.report.txt \
      /out/comparison.md; \
    python3 /work/scripts/gif_frame_analyzer.py /out/evp.gif \
      --fps 50 --json > /out/evp.gif.analysis.json; \
    python3 /work/scripts/gif_frame_analyzer.py /out/vhs.gif \
      --fps 50 --json > /out/vhs.gif.analysis.json; \
    # Verify the results and enforce non-empty outputs internally
    test -s /out/evp.gif; \
    test -s /out/vhs.gif; \
    test -s /out/evp.stats

# Export stage to get all comparison files back
FROM scratch
COPY --from=0 /out/ /
