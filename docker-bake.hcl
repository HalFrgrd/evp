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
variable "BUILD_ENV_TAG"        { default = "evp-build-env:local" }
variable "BUILD_ENV_IMAGE"      { default = "build-env" }
variable "EXTRACT_BINARY_DEST"  { default = "/tmp/evp-build" }

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

target "build-env" {
  inherits   = ["_common"]
  dockerfile = "docker/build-env.Dockerfile"
  tags       = [BUILD_ENV_TAG]
}

target "builder" {
  inherits   = ["_common"]
  dockerfile = "docker/builder.Dockerfile"
  args = {
    BUILD_ENV_IMAGE = BUILD_ENV_IMAGE
  }
  contexts = {
    build-env = "target:build-env"
  }
  tags = ["evp-builder:local"]
}

target "test" {
  inherits   = ["_common"]
  dockerfile = "docker/test.Dockerfile"
  contexts = {
    builder = "target:builder"
  }
  tags = ["evp-test:local"]
}

target "torture" {
  inherits   = ["_common"]
  dockerfile = "docker/torture.Dockerfile"
  contexts = {
    builder = "target:builder"
  }
  tags = ["evp-torture:local"]
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
