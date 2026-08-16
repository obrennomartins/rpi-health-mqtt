#!/usr/bin/env bash

set -euo pipefail

fail() {
    echo "Release helper test failed: $*" >&2
    exit 1
}

assert_file_contains() {
    local file=$1
    local expected=$2
    local description=$3

    grep -F -- "${expected}" "${file}" >/dev/null ||
        fail "${description}"
}

version="$(bash scripts/verify-release-tag.sh)"
bash scripts/verify-release-tag.sh "v${version}" >/dev/null

release_workflow=.github/workflows/release.yml
[[ "$(grep -F -c '          fetch-depth: 0' "${release_workflow}")" -eq 2 ]] ||
    fail "both release checkouts must fetch complete Git history"
assert_file_contains "${release_workflow}" 'bash scripts/validate-ci.sh' "release publication must be gated by CI security validation"
# GitHub expression syntax must remain literal in the expected workflow text.
# shellcheck disable=SC2016
assert_file_contains "${release_workflow}" 'sbom-path: dist/rpi-health-mqtt-${{ needs.package.outputs.version }}-armv7-unknown-linux-gnueabihf.spdx.json' "SBOM attestation must use an exact file path"
# Shell expansion syntax must remain literal in the expected workflow text.
# shellcheck disable=SC2016
assert_file_contains "${release_workflow}" 'precedence_version="${RELEASE_VERSION%%+*}"' "prerelease detection must ignore build metadata"
assert_file_contains scripts/validate-release.sh '--target verify-sbom' "the local release gate must execute real SBOM verification"
assert_file_contains docker/release.Dockerfile 'FROM package AS verify-sbom' "the release Dockerfile must define the SBOM verification stage"
assert_file_contains docker/release.Dockerfile 'syft_version=1.42.3;' "the SBOM verification stage must pin the tested Syft version"
assert_file_contains docker/release.Dockerfile 'syft_sha256=0d6be741479eddd2c8644a288990c04f3df0d609bbc1599a005532a9dff63509;' "the SBOM verification stage must pin the Syft archive checksum"

is_prerelease_version() {
    local precedence_version=${1%%+*}
    [[ "${precedence_version}" == *-* ]]
}

for stable_version in 1.2.3 1.2.3+build-test; do
    if is_prerelease_version "${stable_version}"; then
        fail "stable version was classified as a prerelease: ${stable_version}"
    fi
done
for prerelease_version in 1.2.3-rc.1 1.2.3-rc.1+build-test; do
    if ! is_prerelease_version "${prerelease_version}"; then
        fail "prerelease version was classified as stable: ${prerelease_version}"
    fi
done

invalid_tags=(
    "${version}"
    v1.2
    v1.2.3.4
    v01.2.3
    v1.02.3
    v1.2.03
    v1.2.3-01
    v1.2.3+
)

for invalid_tag in "${invalid_tags[@]}"; do
    if bash scripts/verify-release-tag.sh "${invalid_tag}" >/dev/null 2>&1; then
        fail "invalid tag was accepted: ${invalid_tag}"
    fi
done

if bash scripts/verify-release-tag.sh one two >/dev/null 2>&1; then
    fail "extra arguments were accepted"
fi

if bash scripts/verify-release-tag.sh \
    "v${version}" \
    --main-ref invalid-reference \
    >/dev/null 2>&1; then
    fail "an invalid main branch reference was accepted"
fi

temporary_directory="$(mktemp -d)"
cleanup() {
    rm -rf -- "${temporary_directory}"
}
trap cleanup EXIT

test_repository="${temporary_directory}/repository"
mkdir -p "${test_repository}/scripts"
cp Cargo.toml "${test_repository}/Cargo.toml"
cp scripts/verify-release-tag.sh "${test_repository}/scripts/verify-release-tag.sh"

(
    cd "${test_repository}"
    git init --quiet --initial-branch=test-root
    git config user.name "Release test"
    git config user.email "release-test@example.invalid"
    git add Cargo.toml scripts/verify-release-tag.sh
    git commit --quiet -m "Create test release"
    git tag "v${version}"
    git update-ref refs/remotes/origin/main HEAD
    bash scripts/verify-release-tag.sh \
        "v${version}" \
        --main-ref refs/remotes/origin/main \
        >/dev/null

    printf '\n# second main-branch commit\n' >> Cargo.toml
    git add Cargo.toml
    git commit --quiet -m "Advance the test main branch"
    git update-ref refs/remotes/origin/main HEAD
    if bash scripts/verify-release-tag.sh \
        "v${version}" \
        --main-ref refs/remotes/origin/main \
        >/dev/null 2>&1; then
        fail "a release tag identifying a different commit was accepted"
    fi

    git switch --quiet --create unmerged
    printf '\n# test-only change\n' >> Cargo.toml
    git add Cargo.toml
    git commit --quiet -m "Create unmerged test commit"
    git tag --force "v${version}" >/dev/null
    if bash scripts/verify-release-tag.sh \
        "v${version}" \
        --main-ref refs/remotes/origin/main \
        >/dev/null 2>&1; then
        fail "a release commit outside the main branch was accepted"
    fi
)

archive_root="rpi-health-mqtt-${version}-armv7-unknown-linux-gnueabihf"
archive_stage="${temporary_directory}/archive-stage/${archive_root}"
archive_files=(
    CONTRIBUTING.md
    LICENSE
    README.md
    SECURITY.md
    THIRD-PARTY-LICENSES.html
    config/config.example.toml
    config/project.markdownlint-cli2.jsonc
    docs/installation-and-configuration.md
    scripts/install.sh
    scripts/uninstall.sh
    systemd/rpi-health-mqtt.service
    target/armv7-unknown-linux-gnueabihf/release/rpi-health-mqtt
)

for relative_path in "${archive_files[@]}"; do
    install -D -m 0644 /dev/null "${archive_stage}/${relative_path}"
done
chmod 0755 \
    "${archive_stage}/scripts/install.sh" \
    "${archive_stage}/scripts/uninstall.sh" \
    "${archive_stage}/target/armv7-unknown-linux-gnueabihf/release/rpi-health-mqtt"

valid_archive_directory="${temporary_directory}/valid-archive"
mkdir -p "${valid_archive_directory}"
valid_archive="${valid_archive_directory}/${archive_root}.tar.gz"
tar -czf "${valid_archive}" \
    -C "${temporary_directory}/archive-stage" \
    "${archive_root}"
bash scripts/verify-release-archive.sh "${valid_archive}" "${version}" >/dev/null

linked_support_files=(
    CONTRIBUTING.md
    SECURITY.md
    config/project.markdownlint-cli2.jsonc
)
for relative_path in "${linked_support_files[@]}"; do
    invalid_archive_directory="${temporary_directory}/missing-${relative_path//\//-}"
    mkdir -p "${invalid_archive_directory}"
    invalid_archive="${invalid_archive_directory}/${archive_root}.tar.gz"
    tar -czf "${invalid_archive}" \
        --exclude="${archive_root}/${relative_path}" \
        -C "${temporary_directory}/archive-stage" \
        "${archive_root}"
    if bash scripts/verify-release-archive.sh \
        "${invalid_archive}" \
        "${version}" \
        >/dev/null 2>&1; then
        fail "an archive missing ${relative_path} was accepted"
    fi
done

fake_syft="${temporary_directory}/fake-syft"
cat > "${fake_syft}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
output=
for argument in "$@"; do
    case "${argument}" in
        spdx-json=*) output=${argument#spdx-json=} ;;
    esac
done
[[ -n "${output}" ]]
printf '%s\n' \
    '{' \
    '  "spdxVersion": "SPDX-2.3",' \
    '  "dataLicense": "CC0-1.0",' \
    '  "SPDXID": "SPDXRef-DOCUMENT",' \
    '  "name": "rpi-health-mqtt",' \
    "  \"documentNamespace\": \"https://invalid.example/$RANDOM\"," \
    '  "creationInfo": {' \
    "    \"created\": \"$(date --utc '+%Y-%m-%dT%H:%M:%SZ')\"," \
    '    "creators": ["Tool: test"]' \
    '  },' \
    '  "packages": []' \
    '}' \
    > "${output}"
EOF
chmod 0755 "${fake_syft}"

sbom_input="${temporary_directory}/sbom-input"
mkdir -p "${sbom_input}"
archive="${temporary_directory}/rpi-health-mqtt-${version}-armv7-unknown-linux-gnueabihf.tar.gz"
printf 'test archive\n' > "${archive}"

for output in one two; do
    bash scripts/generate-release-sbom.sh \
        "${fake_syft}" \
        "${archive}" \
        "${version}" \
        1 \
        "${sbom_input}" \
        "${temporary_directory}/${output}.spdx.json" \
        example/example
done

cmp "${temporary_directory}/one.spdx.json" \
    "${temporary_directory}/two.spdx.json"
jq --exit-status \
    '.creationInfo.created == "1970-01-01T00:00:01Z"
     and (.documentNamespace | contains("sha256="))' \
    "${temporary_directory}/one.spdx.json" \
    >/dev/null

if bash scripts/generate-release-sbom.sh \
    "${fake_syft}" \
    "${archive}" \
    "${version}" \
    1 \
    "${sbom_input}" \
    "${temporary_directory}/invalid.spdx.json" \
    invalid-repository \
    >/dev/null 2>&1; then
    fail "an invalid repository name was accepted for the SBOM namespace"
fi

if bash scripts/verify-release-archive.sh one two three >/dev/null 2>&1; then
    fail "archive validator accepted extra arguments"
fi

echo "Release helper tests passed."
