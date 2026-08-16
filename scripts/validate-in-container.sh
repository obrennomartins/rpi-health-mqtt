#!/usr/bin/env bash

set -euo pipefail

mode="${1:?validation mode is required}"

case "${mode}" in
  amd64)
    shellcheck \
      scripts/install.sh \
      scripts/uninstall.sh \
      scripts/validate-ci-in-container.sh \
      scripts/validate-ci.sh \
      scripts/validate-delivery.sh \
      scripts/validate-docs.sh \
      scripts/validate-in-container.sh \
      tests/install/fake-dpkg.sh \
      tests/install/fake-systemctl.sh \
      tests/install/fake-uname.sh \
      tests/install/run.sh \
      tests/install/verify-systemd-unit.sh
    DOCS_REQUIRE_CODESPELL=1 bash scripts/validate-docs.sh
    cargo fmt --all -- --check
    cargo test --all-targets --all-features --locked
    cargo clippy --all-targets --all-features --locked -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked
    installer_target="armv7-unknown-linux-gnueabihf"
    cargo build --locked --release --target "${installer_target}"
    INSTALL_TEST_CONTAINER=1 \
      INSTALL_TEST_ARM_BINARY="target/${installer_target}/release/rpi-health-mqtt" \
      sh tests/install/run.sh
    INSTALL_TEST_CONTAINER=1 sh tests/install/verify-systemd-unit.sh
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
