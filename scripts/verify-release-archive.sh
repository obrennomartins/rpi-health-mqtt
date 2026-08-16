#!/usr/bin/env bash

set -euo pipefail

usage() {
    echo "Usage: scripts/verify-release-archive.sh ARCHIVE VERSION" >&2
}

fail() {
    echo "Release archive validation failed: $*" >&2
    exit 1
}

if [[ $# -ne 2 ]]; then
    usage
    exit 2
fi

archive=$1
version=$2
target=armv7-unknown-linux-gnueabihf
archive_root="rpi-health-mqtt-${version}-${target}"

[[ -f "${archive}" && -r "${archive}" ]] \
    || fail "archive is not a readable regular file: ${archive}"

while IFS= read -r member; do
    case "${member}" in
        /*|../*|*/../*|*\\*)
            fail "unsafe archive member path: ${member}"
            ;;
        "${archive_root}"|"${archive_root}/"|"${archive_root}/"*)
            ;;
        *)
            fail "archive member is outside the expected root: ${member}"
            ;;
    esac
done < <(tar -tzf "${archive}")

while IFS= read -r entry; do
    case "${entry:0:1}" in
        -|d)
            ;;
        *)
            fail "archive contains a link or special file: ${entry}"
            ;;
    esac
done < <(tar -tvzf "${archive}")

extraction_directory="$(mktemp -d)"
cleanup() {
    rm -rf -- "${extraction_directory}"
}
trap cleanup EXIT

tar -xzf "${archive}" -C "${extraction_directory}"
package_root="${extraction_directory}/${archive_root}"
[[ -d "${package_root}" ]] || fail "expected archive root is missing"

expected_files="$({
    printf '%s\n' \
        CONTRIBUTING.md \
        LICENSE \
        README.md \
        SECURITY.md \
        THIRD-PARTY-LICENSES.html \
        config/config.example.toml \
        config/project.markdownlint-cli2.jsonc \
        docs/installation-and-configuration.md \
        scripts/install.sh \
        scripts/uninstall.sh \
        systemd/rpi-health-mqtt.service \
        "target/${target}/release/rpi-health-mqtt"
} | LC_ALL=C sort)"
actual_files="$(find "${package_root}" -type f -printf '%P\n' | LC_ALL=C sort)"
[[ "${actual_files}" == "${expected_files}" ]] \
    || fail "archive content does not match the release manifest"

if find "${package_root}" -type l -print -quit | grep -q .; then
    fail "archive contains a symbolic link"
fi

while IFS= read -r directory; do
    [[ "$(stat -c '%a' "${directory}")" == 755 ]] \
        || fail "directory mode is not 0755: ${directory#"${package_root}"/}"
done < <(find "${package_root}" -type d -print)

while IFS= read -r file; do
    relative_path=${file#"${package_root}"/}
    expected_mode=644
    case "${relative_path}" in
        scripts/install.sh|scripts/uninstall.sh|target/*/release/rpi-health-mqtt)
            expected_mode=755
            ;;
    esac
    [[ "$(stat -c '%a' "${file}")" == "${expected_mode}" ]] \
        || fail "unexpected mode for ${relative_path}"
done < <(find "${package_root}" -type f -print)

binary="${package_root}/target/${target}/release/rpi-health-mqtt"
if strings "${binary}" \
    | grep -E '(C:\\Users\\|/Users/[^/]+|/home/runner|/workspace-(one|two)|/usr/local/cargo)' \
    >/dev/null; then
    fail "binary contains a local build path"
fi

if grep -R -a -E 'C:\\Users\\[^\\]+' "${package_root}" >/dev/null; then
    fail "archive contains a Windows user profile path"
fi

echo "Release archive validation passed."
