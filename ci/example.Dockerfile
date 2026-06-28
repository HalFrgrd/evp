# syntax=docker/dockerfile:1.7
FROM ubuntu:26.04

# Install standard commands, fonts, or tools.
RUN apt-get update && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        && rm -rf /var/lib/apt/lists/*

# Copy the evp binary and evp_helper_tool from the evp-binary build context
COPY --from=evp-binary /evp /usr/local/bin/evp
COPY --from=evp-binary /evp_helper_tool /usr/local/bin/evp_helper_tool

WORKDIR /workspace

# Copy the examples directory
COPY examples/ /workspace/examples/

# Set up the expected helper tool path for mouse.tape
RUN mkdir -p /workspace/target/x86_64-unknown-linux-musl/release && \
    ln -s /usr/local/bin/evp_helper_tool /workspace/target/x86_64-unknown-linux-musl/release/evp_helper_tool

# Set environment variables
ARG BUILDKIT_SANDBOX_HOSTNAME
ENV HOSTNAME=${BUILDKIT_SANDBOX_HOSTNAME}
ENV PS1="\[\e[38;2;90;86;224m\]> \[\e[0m\]"
RUN echo "export PS1=\"\\[\\e[38;2;90;86;224m\\]> \\[\\e[0m\\]\"" >> ~/.bashrc

ARG EXAMPLE_NAME

# Run evp to build the specific example (outputting both gif and svg)
# We override the tape output files using --output to output both gif and svg!
RUN evp examples/${EXAMPLE_NAME}.tape --output /workspace/${EXAMPLE_NAME}.gif --output /workspace/${EXAMPLE_NAME}.svg --output /workspace/${EXAMPLE_NAME}.stats

# Export stage
FROM scratch
COPY --from=0 /workspace/*.gif /
COPY --from=0 /workspace/*.svg /
COPY --from=0 /workspace/*.stats /
