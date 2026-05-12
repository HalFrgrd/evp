# docker-bake.hcl

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

variable "RUNTIME_TAG"          { default = "evp:local" }
variable "EXTRACT_BINARY_DEST"  { default = "/tmp/evp-build" }
variable "EXTRACT_LIBGHOSTTY_DEST" { default = "assets/libghostty" }
# Copilot setup pre-pulls this tag so buildx can reuse the local cached image.
variable "BUILD_ENV_IMAGE"      { default = "ghcr.io/halfrgrd/evp-build-env:latest" }
variable "TEST_TARGET"          { default = "x86_64-unknown-linux-gnu" }

target "_common" {
  context = "."
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

target "builder" {
  inherits   = ["_common"]
  dockerfile = "docker/builder.Dockerfile"
  contexts = {
    libghostty = "./assets/libghostty"
    build-env  = "docker-image://${BUILD_ENV_IMAGE}"
  }
  tags = ["evp-builder:local"]
}

target "test" {
  inherits   = ["_common"]
  dockerfile = "docker/test.Dockerfile"
  contexts = {
    builder = "target:builder-test"
  }
  tags = ["evp-test:local"]
}

target "builder-test" {
  inherits = ["builder"]
  args = {
    TARGET = TEST_TARGET
  }
  tags = ["evp-builder-test:local"]
}

target "stress_test" {
  inherits   = ["_common"]
  dockerfile = "docker/stress_test.Dockerfile"
  contexts = {
    builder = "target:builder"
  }
  tags = ["evp-stress_test:local"]
}

target "runtime" {
  inherits   = ["_common"]
  dockerfile = "docker/runtime.Dockerfile"
  contexts = {
    builder = "target:builder"
  }
  tags = [RUNTIME_TAG]
}

target "extract-binary" {
  inherits   = ["_common"]
  dockerfile = "docker/binary.Dockerfile"
  contexts = {
    builder = "target:builder"
  }
  output = ["type=local,dest=${EXTRACT_BINARY_DEST}"]
}

target "extract-libghostty" {
  inherits   = ["_common"]
  dockerfile = "docker/libghostty-pkgconfig.Dockerfile"
  output     = ["type=local,dest=${EXTRACT_LIBGHOSTTY_DEST}"]
}
