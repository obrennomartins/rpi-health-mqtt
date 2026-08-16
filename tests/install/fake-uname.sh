#!/bin/sh
set -eu

if [ "$#" -eq 1 ] && [ "$1" = -m ] && [ -n "${INSTALL_TEST_UNAME_MACHINE:-}" ]; then
    printf '%s\n' "$INSTALL_TEST_UNAME_MACHINE"
    exit 0
fi

exec /usr/bin/uname "$@"
