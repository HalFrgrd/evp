# EVP in Continuous Integration (CI)

This document describes how to run and integrate EVP in your CI workflows, automated pipelines, and Docker containers.

---

## GitHub Actions

You can run `evp` directly in your GitHub Actions workflows to automate rendering your terminal recordings. Since the action downloads a prebuilt static binary, no Rust toolchain or Docker setup is required:

```yaml
- name: Render tape script
  uses: HalFrgrd/evp@v0.15.0 # Replace with the desired release tag
  with:
    script: demo.tape
    output: demo.gif
```

---

## Docker

EVP publishes a pre-configured Docker image containing the statically linked `evp` binary on a modern Ubuntu base.

### Interactive Demo Sandbox
To play around in the sandbox with a customized purple shell prompt and preloaded samples:
```bash
docker run -it ghcr.io/halfrgrd/evp:latest
```
Inside the container, you will find demo folders and a sample script ready to run:
```bash
cd ~/demos
evp demo.tape --output demo.gif
```

### Rendering Local Scripts
To render your local `.tape` scripts without installing `evp` on your host system, mount your current working directory:
```bash
docker run --rm -v "$(pwd)":/work -w /work ghcr.io/halfrgrd/evp:latest evp my_script.tape
```

### Custom Docker Builds
For custom environments, you can use the pre-built static binaries in your own multi-stage Dockerfiles:
```dockerfile
FROM ghcr.io/halfrgrd/evp:latest AS evp
FROM ubuntu:latest
COPY --from=evp /usr/local/bin/evp /usr/local/bin/evp
# Add other dependencies and your script...
```
