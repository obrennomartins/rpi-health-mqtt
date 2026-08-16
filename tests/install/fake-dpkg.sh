#!/bin/sh
set -eu

if [ "$#" -eq 1 ] &&
    [ "$1" = --print-architecture ] &&
    [ -n "${INSTALL_TEST_DPKG_ARCHITECTURE:-}" ]; then
    printf '%s\n' "$INSTALL_TEST_DPKG_ARCHITECTURE"
    exit 0
fi

exec /usr/bin/dpkg "$@"
