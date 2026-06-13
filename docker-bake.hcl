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
  contexts = {
    libghostty = "target:extract-libghostty"
  }
  args = {
    VERGEN_GIT_SHA         = VERGEN_GIT_SHA
    VERGEN_GIT_BRANCH      = VERGEN_GIT_BRANCH
    VERGEN_GIT_COMMIT_DATE = VERGEN_GIT_COMMIT_DATE
    VERGEN_GIT_DIRTY       = VERGEN_GIT_DIRTY
  }
  output     = ["type=local,dest=${EXTRACT_EVP_DEST}"]
}

group "examples" {
  targets = [
    "example-char_spacing",
    "example-colors",
    "example-embedded-font-demo",
    "example-evp_demo",
    "example-hello",
    "example-keys",
    "example-mouse",
    "example-my_program_demo",
    "example-progress",
    "example-shell-tour"
  ]
}

target "example-base" {
  context    = "."
  dockerfile = "ci/example.Dockerfile"
  contexts = {
    evp-binary = "target:evp"
  }
  args = {
    BUILDKIT_SANDBOX_HOSTNAME = "my-hostname"
  }
}

variable "EXTRACT_EXAMPLES_DEST" { default = "ci/examples" }

target "example-char_spacing" {
  inherits = ["example-base"]
  args     = { EXAMPLE_NAME = "char_spacing" }
  output   = ["type=local,dest=${EXTRACT_EXAMPLES_DEST}/char_spacing"]
}

target "example-colors" {
  inherits = ["example-base"]
  args     = { EXAMPLE_NAME = "colors" }
  output   = ["type=local,dest=${EXTRACT_EXAMPLES_DEST}/colors"]
}

target "example-embedded-font-demo" {
  inherits = ["example-base"]
  args     = { EXAMPLE_NAME = "embedded-font-demo" }
  output   = ["type=local,dest=${EXTRACT_EXAMPLES_DEST}/embedded-font-demo"]
}

target "example-evp_demo" {
  inherits = ["example-base"]
  args     = { EXAMPLE_NAME = "evp_demo" }
  output   = ["type=local,dest=${EXTRACT_EXAMPLES_DEST}/evp_demo"]
}

target "example-hello" {
  inherits = ["example-base"]
  args     = { EXAMPLE_NAME = "hello" }
  output   = ["type=local,dest=${EXTRACT_EXAMPLES_DEST}/hello"]
}

target "example-keys" {
  inherits = ["example-base"]
  args     = { EXAMPLE_NAME = "keys" }
  output   = ["type=local,dest=${EXTRACT_EXAMPLES_DEST}/keys"]
}

target "example-mouse" {
  inherits = ["example-base"]
  args     = { EXAMPLE_NAME = "mouse" }
  output   = ["type=local,dest=${EXTRACT_EXAMPLES_DEST}/mouse"]
}

target "example-my_program_demo" {
  inherits = ["example-base"]
  args     = { EXAMPLE_NAME = "my_program_demo" }
  output   = ["type=local,dest=${EXTRACT_EXAMPLES_DEST}/my_program_demo"]
}

target "example-progress" {
  inherits = ["example-base"]
  args     = { EXAMPLE_NAME = "progress" }
  output   = ["type=local,dest=${EXTRACT_EXAMPLES_DEST}/progress"]
}

target "example-shell-tour" {
  inherits = ["example-base"]
  args     = { EXAMPLE_NAME = "shell-tour" }
  output   = ["type=local,dest=${EXTRACT_EXAMPLES_DEST}/shell-tour"]
}

