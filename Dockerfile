# syntax=docker/dockerfile:1.7
FROM ubuntu:24.04

# Avoid prompts during package installation
ENV DEBIAN_FRONTEND=noninteractive

# Install basic command-line utilities
RUN apt-get update && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        curl \
        git \
        nano \
        vim \
    && rm -rf /var/lib/apt/lists/*

# Copy the static evp binary and helper tool from the evp-binary build context
COPY --from=evp-binary /evp /usr/bin/evp
COPY --from=evp-binary /evp_helper_tool /usr/bin/evp_helper_tool

# Ensure binaries have execution permissions and are on PATH
RUN chmod +x /usr/bin/evp /usr/bin/evp_helper_tool
ENV PATH="/usr/bin:${PATH}"

# Set PS1 to be a purple "> " for bash shells
ENV PS1="\[\e[38;2;168;85;247m\]> \[\e[0m\]"
RUN echo 'export PS1="\[\e[38;2;168;85;247m\]> \[\e[0m\]"' >> /root/.bashrc

# Create dummy folders and files in the user's home folder (/root)
RUN mkdir -p /root/demos /root/projects /root/docs /root/recordings

# Create a README.md file in the home directory
RUN cat <<'EOF' > /root/README.md
# Welcome to the evp Demo Environment!

This container comes pre-configured with `evp` on your PATH.

## Getting Started

1. Go to the `demos` directory:
   ```bash
   cd ~/demos
   ```

2. Run the sample tape script to generate a GIF:
   ```bash
   evp demo.tape --output demo.gif
   ```

3. Explore the dummy folders and files in your home folder.
EOF

# Create a dummy tape file in /root/demos/demo.tape
RUN cat <<'EOF' > /root/demos/demo.tape
# A sample tape script to demonstrate evp
Output demo.gif

Set Shell bash
Set Width 800
Set Height 400
Set FontSize 20
Set TypingSpeed 60ms
Set Framerate 30

Sleep 1s
Type "echo 'Welcome to evp!'"
Sleep 500ms
Enter
Sleep 1s
Type "ls -F ~"
Sleep 500ms
Enter
Sleep 2s
EOF

# Create some dummy files in projects and docs
RUN echo "This is a dummy project file." > /root/projects/app.js && \
    echo "This is a configuration example." > /root/projects/config.json && \
    echo "Documentation placeholder." > /root/docs/guide.md

WORKDIR /root
CMD ["/bin/bash"]
