#!/bin/sh
set -eu

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

[ "$(id -u)" -eq 0 ] || fail "systemd unit verification requires root"
[ "${INSTALL_TEST_CONTAINER:-}" = 1 ] ||
    fail "INSTALL_TEST_CONTAINER=1 is required"
[ -f /run/rpi-health-mqtt-test-container ] ||
    fail "systemd unit verification must run in a Docker container"

TEST_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
PROJECT_ROOT=$(CDPATH='' cd -- "$TEST_DIR/../.." && pwd -P)
UNIT_FILE="$PROJECT_ROOT/systemd/rpi-health-mqtt.service"
VERIFICATION_UNIT=/etc/systemd/system/rpi-health-mqtt.service
EXECUTABLE=/usr/local/bin/rpi-health-mqtt
CONFIG_DIR=/etc/rpi-health-mqtt

if [ -e "$EXECUTABLE" ] || [ -L "$EXECUTABLE" ] ||
    [ -e "$VERIFICATION_UNIT" ] || [ -L "$VERIFICATION_UNIT" ] ||
    [ -e "$CONFIG_DIR" ] || [ -L "$CONFIG_DIR" ]; then
    fail "isolated container contains an unexpected managed path"
fi

cleanup() {
    rm -f -- "$EXECUTABLE" "$VERIFICATION_UNIT"
    rmdir -- "$CONFIG_DIR" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

install -d -o root -g root -m 0755 -- "$(dirname -- "$EXECUTABLE")"
install -o root -g root -m 0755 -- /bin/true "$EXECUTABLE"
install -d -o root -g root -m 0750 -- "$CONFIG_DIR"
install -o root -g root -m 0644 -- "$UNIT_FILE" "$VERIFICATION_UNIT"

systemd-analyze verify "$VERIFICATION_UNIT"
printf 'systemd unit verification passed.\n'
