# docker-bake.hcl
#
# One-shot build orchestration for evp.
#
# Common invocations (run from the repo root — buildx auto-detects
# `./docker-bake.hcl`):
#
#   # Build a static evp binary and drop it into docker/build/evp:
#   docker buildx bake extract-binary
#
#   # Build the runtime container image (tagged `evp:local`):
#   docker buildx bake runtime
#
#   # Run the workspace test suite the same way CI does:
#   docker buildx bake test
#
#   # Just produce the builder image (intermediate, useful for poking
#   # around with `docker run --rm -it evp-builder:local`):
#   docker buildx bake builder

# Vergen build args. Default to "unknown" so local builds don't have to
# know git state; CI populates these with real values via `--set`.
variable "VERGEN_GIT_SHA"                  { default = "unknown" }
variable "VERGEN_GIT_BRANCH"               { default = "unknown" }
variable "VERGEN_GIT_COMMIT_DATE"          { default = "unknown" }
variable "VERGEN_GIT_COMMIT_TIMESTAMP"     { default = "unknown" }
variable "VERGEN_GIT_COMMIT_COUNT"         { default = "unknown" }
variable "VERGEN_GIT_COMMIT_AUTHOR_NAME"   { default = "unknown" }
variable "VERGEN_GIT_COMMIT_AUTHOR_EMAIL"  { default = "unknown" }
variable "VERGEN_GIT_COMMIT_MESSAGE"       { default = "unknown" }
variable "VERGEN_GIT_DESCRIBE"             { default = "unknown" }
variable "VERGEN_GIT_DIRTY"                { default = "unknown" }

# Default tag for the runtime image when bake is invoked locally. CI
# overrides via `--set runtime.tags=...`.
variable "RUNTIME_TAG" { default = "evp:local" }

# Shared build args block applied to every target.
target "_common" {
    context    = "."
    dockerfile = "docker/Dockerfile"
    args = {
        VERGEN_GIT_SHA                 = VERGEN_GIT_SHA
        VERGEN_GIT_BRANCH              = VERGEN_GIT_BRANCH
        VERGEN_GIT_COMMIT_DATE         = VERGEN_GIT_COMMIT_DATE
        VERGEN_GIT_COMMIT_TIMESTAMP    = VERGEN_GIT_COMMIT_TIMESTAMP
        VERGEN_GIT_COMMIT_COUNT        = VERGEN_GIT_COMMIT_COUNT
        VERGEN_GIT_COMMIT_AUTHOR_NAME  = VERGEN_GIT_COMMIT_AUTHOR_NAME
        VERGEN_GIT_COMMIT_AUTHOR_EMAIL = VERGEN_GIT_COMMIT_AUTHOR_EMAIL
        VERGEN_GIT_COMMIT_MESSAGE      = VERGEN_GIT_COMMIT_MESSAGE
        VERGEN_GIT_DESCRIBE            = VERGEN_GIT_DESCRIBE
        VERGEN_GIT_DIRTY               = VERGEN_GIT_DIRTY
    }
}

# Stops at the builder stage. Useful for poking around with `docker run`
# or as an upstream for `extract-binary` / other targets that just need
# the compiled binary.
target "builder" {
    inherits = ["_common"]
    target   = "builder"
    tags     = ["evp-builder:local"]
}

# Runs `cargo build --workspace && cargo test --workspace` inside the
# build environment. Used by ci.yml.
target "test" {
    inherits = ["_common"]
    target   = "test"
    tags     = ["evp-test:local"]
}

# The publishable runtime image: debian-slim + the static evp binary.
# Used by docker.yml.
target "runtime" {
    inherits = ["_common"]
    target   = "runtime"
    tags     = [RUNTIME_TAG]
}

# Extracts the static evp binary onto the host filesystem at
# `docker/build/evp`. This is the one-command "give me a working evp"
# entrypoint for local builds and for release.yml.
#
# `output = "type=local,..."` makes buildx write the contents of the
# `binary` scratch stage to the host instead of producing an image, so
# there's no `docker create` / `docker cp` plumbing in user workflows.
target "extract-binary" {
    inherits = ["_common"]
    target   = "binary"
    output   = ["type=local,dest=docker/build"]
}
