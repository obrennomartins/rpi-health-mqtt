#!/usr/bin/env bash

set -euo pipefail

mode="${1:?validation mode is required}"

case "${mode}" in
  amd64)
    cargo fmt --all -- --check
    cargo test --all-targets --all-features --locked
    cargo clippy --all-targets --all-features --locked -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked
    ;;
  armv7)
    target="armv7-unknown-linux-gnueabihf"
    cargo test --all-targets --all-features --locked --target "${target}" -- --test-threads=1
    cargo clippy --all-targets --all-features --locked --target "${target}" -- -D warnings
    cargo build --locked --release --target "${target}"

    binary="target/${target}/release/rpi-health-mqtt"
    file "${binary}" | grep -Eq 'ELF 32-bit LSB.*ARM'
    arm-linux-gnueabihf-readelf -h "${binary}" | grep -Eq 'Class:[[:space:]]+ELF32'
    arm-linux-gnueabihf-readelf -h "${binary}" | grep -Eq 'Machine:[[:space:]]+ARM'
    arm-linux-gnueabihf-readelf -A "${binary}" | grep -Eq 'Tag_ABI_VFP_args: VFP registers'
    qemu-arm-static -L /usr/arm-linux-gnueabihf "${binary}" --version
    ;;
  *)
    echo "Unknown validation mode: ${mode}" >&2
    exit 2
    ;;
esac
