FROM ghcr.io/charmbracelet/vhs:v0.10.0

ENV HOME=/tmp \
    XDG_CONFIG_HOME=/tmp/chrome-config \
    XDG_CACHE_HOME=/tmp/chrome-cache \
    XDG_RUNTIME_DIR=/tmp/chrome-runtime

COPY scripts/stress_test_program.py /tmp/stress_test_program.py
COPY scripts/stress_test.tape /stress_test.tape

WORKDIR /work
CMD ["/stress_test.tape"]
