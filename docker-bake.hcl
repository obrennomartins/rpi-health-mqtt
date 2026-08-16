group "verify" {
  targets = ["verify-amd64", "verify-armv7", "verify-systemd-bookworm"]
}

target "common" {
  context    = "."
  dockerfile = "docker/verify.Dockerfile"
  platforms  = ["linux/amd64"]
  secret     = ["id=host_ca,src=.local/docker-ca.cer"]
}

target "verify-amd64" {
  inherits = ["common"]
  target   = "verify-amd64"
  tags     = ["rpi-health-mqtt-verify-amd64:local"]
}

target "verify-armv7" {
  inherits = ["common"]
  target   = "verify-armv7"
  tags     = ["rpi-health-mqtt-verify-armv7:local"]
}

target "verify-systemd-bookworm" {
  inherits = ["common"]
  target   = "verify-systemd-bookworm"
  tags     = ["rpi-health-mqtt-verify-systemd-bookworm:local"]
}
