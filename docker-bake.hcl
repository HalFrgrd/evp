# docker-bake.hcl

variable "EXTRACT_LIBGHOSTTY_DEST" { default = "assets/libghostty" }

target "extract-libghostty" {
  context    = "."
  dockerfile = "docker/libghostty-pkgconfig.Dockerfile"
  output     = ["type=local,dest=${EXTRACT_LIBGHOSTTY_DEST}"]
}
