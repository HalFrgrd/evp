# docker-bake.hcl

group "default" {
  targets = ["evp"]
}

variable "EXTRACT_LIBGHOSTTY_DEST" { default = "assets/libghostty" }

target "extract-libghostty" {
  context    = "."
  dockerfile = "ci/libghostty-pkgconfig.Dockerfile"
  output     = ["type=local,dest=${EXTRACT_LIBGHOSTTY_DEST}"]
}

target "vhs" {
  context    = "."
  dockerfile = "ci/vhs.Dockerfile"
  tags       = ["evp-vhs:latest"]
}

variable "EXTRACT_EVP_DEST" { default = "ci/build" }

variable "VERGEN_GIT_SHA" { default = "" }
variable "VERGEN_GIT_BRANCH" { default = "" }
variable "VERGEN_GIT_COMMIT_DATE" { default = "" }
variable "VERGEN_GIT_DIRTY" { default = "" }

target "evp" {
  context    = "."
  dockerfile = "ci/evp.Dockerfile"
  args = {
    VERGEN_GIT_SHA         = VERGEN_GIT_SHA
    VERGEN_GIT_BRANCH      = VERGEN_GIT_BRANCH
    VERGEN_GIT_COMMIT_DATE = VERGEN_GIT_COMMIT_DATE
    VERGEN_GIT_DIRTY       = VERGEN_GIT_DIRTY
  }
  output     = ["type=local,dest=${EXTRACT_EVP_DEST}"]
}

