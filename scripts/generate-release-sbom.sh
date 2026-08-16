#!/usr/bin/env bash

set -euo pipefail

usage() {
    echo "Usage: scripts/generate-release-sbom.sh SYFT ARCHIVE VERSION SOURCE_DATE_EPOCH INPUT_DIRECTORY OUTPUT_FILE REPOSITORY" >&2
}

fail() {
    echo "SBOM generation failed: $*" >&2
    exit 1
}

if [[ $# -ne 7 ]]; then
    usage
    exit 2
fi

syft_executable=$1
archive=$2
version=$3
source_date_epoch=$4
input_directory=$5
output_file=$6
repository=$7
target=armv7-unknown-linux-gnueabihf
expected_archive_name="rpi-health-mqtt-${version}-${target}.tar.gz"

[[ -x "${syft_executable}" ]] \
    || fail "Syft is not an executable file: ${syft_executable}"
[[ -f "${archive}" && -r "${archive}" ]] \
    || fail "release archive is not a readable regular file: ${archive}"
[[ "$(basename -- "${archive}")" == "${expected_archive_name}" ]] \
    || fail "release archive name does not match the version and target"
[[ -d "${input_directory}" ]] \
    || fail "SBOM input is not a directory: ${input_directory}"
[[ "${source_date_epoch}" =~ ^[0-9]+$ ]] \
    || fail "SOURCE_DATE_EPOCH must be a non-negative integer"
[[ "${repository}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
    || fail "repository must use the owner/name form"
command -v jq >/dev/null 2>&1 || fail "jq is required"

manifest_version="$(bash scripts/verify-release-tag.sh)"
[[ "${version}" == "${manifest_version}" ]] \
    || fail "version does not match Cargo.toml"

if [[ -L "${output_file}" ]]; then
    fail "output file must not be a symbolic link"
fi
mkdir -p "$(dirname -- "${output_file}")"

temporary_directory="$(mktemp -d)"
cleanup() {
    rm -rf -- "${temporary_directory}"
}
trap cleanup EXIT

raw_sbom="${temporary_directory}/raw.spdx.json"
normalized_sbom="${temporary_directory}/normalized.spdx.json"
archive_sha256="$(sha256sum -- "${archive}" | awk '{print $1}')"
created="$(date --utc --date="@${source_date_epoch}" '+%Y-%m-%dT%H:%M:%SZ')" \
    || fail "SOURCE_DATE_EPOCH is outside the supported range"
namespace="https://github.com/${repository}/releases/download/v${version}/${expected_archive_name}?sha256=${archive_sha256}"

SYFT_CHECK_FOR_APP_UPDATE=false "${syft_executable}" scan \
    "dir:${input_directory}" \
    --source-name rpi-health-mqtt \
    --source-version "${version}" \
    --output "spdx-json=${raw_sbom}" \
    --quiet

jq --sort-keys \
    --arg created "${created}" \
    --arg namespace "${namespace}" \
    '.creationInfo.created = $created
     | .documentNamespace = $namespace' \
    "${raw_sbom}" \
    > "${normalized_sbom}"

jq --exit-status \
    --arg created "${created}" \
    --arg namespace "${namespace}" \
    '.spdxVersion == "SPDX-2.3"
     and .creationInfo.created == $created
     and .documentNamespace == $namespace' \
    "${normalized_sbom}" \
    >/dev/null \
    || fail "normalized output is not a valid SPDX 2.3 document"

install -m 0644 "${normalized_sbom}" "${output_file}"
