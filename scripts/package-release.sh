#!/usr/bin/env bash

set -euo pipefail

usage() {
    echo "Usage: scripts/package-release.sh VERSION SOURCE_DATE_EPOCH BINARY OUTPUT_DIRECTORY" >&2
}

if [[ $# -ne 4 ]]; then
    usage
    exit 2
fi

version=$1
source_date_epoch=$2
binary=$3
output_directory=$4
target=armv7-unknown-linux-gnueabihf

manifest_version="$(bash scripts/verify-release-tag.sh)"
if [[ "${version}" != "${manifest_version}" ]]; then
    echo "Package version '${version}' does not match Cargo.toml version '${manifest_version}'." >&2
    exit 1
fi

if [[ ! "${source_date_epoch}" =~ ^[0-9]+$ ]]; then
    echo "SOURCE_DATE_EPOCH must be a non-negative integer." >&2
    exit 1
fi

required_files=(
    CONTRIBUTING.md
    LICENSE
    README.md
    SECURITY.md
    config/config.example.toml
    config/project.markdownlint-cli2.jsonc
    docs/installation-and-configuration.md
    scripts/install.sh
    scripts/uninstall.sh
    systemd/rpi-health-mqtt.service
)

for required_file in "${required_files[@]}"; do
    if [[ ! -f "${required_file}" ]]; then
        echo "Required release file is missing: ${required_file}" >&2
        exit 1
    fi
done

if [[ ! -f "${binary}" || ! -r "${binary}" ]]; then
    echo "Release binary is not a readable regular file: ${binary}" >&2
    exit 1
fi

if [[ -L "${output_directory}" ]]; then
    echo "Output directory must not be a symbolic link: ${output_directory}" >&2
    exit 1
fi

mkdir -p "${output_directory}"
if [[ ! -d "${output_directory}" ]]; then
    echo "Output path is not a directory: ${output_directory}" >&2
    exit 1
fi

stage_directory="$(mktemp -d)"
cleanup() {
    rm -rf -- "${stage_directory}"
}
trap cleanup EXIT

archive_root="rpi-health-mqtt-${version}-${target}"
package_root="${stage_directory}/${archive_root}"
archive_name="${archive_root}.tar.gz"

install -D -m 0755 "${binary}" \
    "${package_root}/target/${target}/release/rpi-health-mqtt"
install -D -m 0644 LICENSE "${package_root}/LICENSE"
install -D -m 0644 README.md "${package_root}/README.md"
install -D -m 0644 CONTRIBUTING.md "${package_root}/CONTRIBUTING.md"
install -D -m 0644 SECURITY.md "${package_root}/SECURITY.md"
install -D -m 0644 config/config.example.toml \
    "${package_root}/config/config.example.toml"
install -D -m 0644 config/project.markdownlint-cli2.jsonc \
    "${package_root}/config/project.markdownlint-cli2.jsonc"
install -D -m 0644 docs/installation-and-configuration.md \
    "${package_root}/docs/installation-and-configuration.md"
install -D -m 0755 scripts/install.sh "${package_root}/scripts/install.sh"
install -D -m 0755 scripts/uninstall.sh "${package_root}/scripts/uninstall.sh"
install -D -m 0644 systemd/rpi-health-mqtt.service \
    "${package_root}/systemd/rpi-health-mqtt.service"

cargo about generate --locked about.hbs \
    > "${package_root}/THIRD-PARTY-LICENSES.html"
if [[ ! -s "${package_root}/THIRD-PARTY-LICENSES.html" ]]; then
    echo "Third-party license report is empty." >&2
    exit 1
fi

find "${package_root}" -exec touch -h -d "@${source_date_epoch}" {} +

tar \
    --sort=name \
    --format=posix \
    --mtime="@${source_date_epoch}" \
    --owner=0 \
    --group=0 \
    --numeric-owner \
    --pax-option=delete=atime,delete=ctime \
    -C "${stage_directory}" \
    -cf - \
    "${archive_root}" \
    | gzip -n -9 > "${output_directory}/${archive_name}"

(
    cd "${output_directory}"
    sha256sum -- "${archive_name}" > "${archive_name}.sha256"
    sha256sum --check "${archive_name}.sha256"
)
