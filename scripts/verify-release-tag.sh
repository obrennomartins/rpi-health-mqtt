#!/usr/bin/env bash

set -euo pipefail

usage() {
    echo "Usage: scripts/verify-release-tag.sh [TAG] [--main-ref REF]" >&2
}

tag=
main_ref=

while [[ $# -gt 0 ]]; do
    case "$1" in
        --main-ref)
            if [[ -n "${main_ref}" || $# -lt 2 || -z "$2" ]]; then
                usage
                exit 2
            fi
            main_ref=$2
            shift 2
            ;;
        -*)
            usage
            exit 2
            ;;
        *)
            if [[ -n "${tag}" ]]; then
                usage
                exit 2
            fi
            tag=$1
            shift
            ;;
    esac
done

manifest_version="$({
    awk '
        /^\[package\][[:space:]]*$/ {
            in_package = 1
            next
        }
        /^\[/ {
            if (in_package) {
                exit
            }
        }
        in_package && /^[[:space:]]*version[[:space:]]*=/ {
            value = $0
            sub(/^[^"]*"/, "", value)
            sub(/".*$/, "", value)
            print value
            exit
        }
    ' Cargo.toml
} || true)"

if [[ -z "${manifest_version}" ]]; then
    echo "Cargo.toml does not contain a package version." >&2
    exit 1
fi

numeric_identifier='(0|[1-9][0-9]*)'
prerelease_identifier='(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)'
semver_pattern="^${numeric_identifier}\\.${numeric_identifier}\\.${numeric_identifier}(-${prerelease_identifier}(\\.${prerelease_identifier})*)?(\\+[0-9A-Za-z-]+(\\.[0-9A-Za-z-]+)*)?$"

if [[ ! "${manifest_version}" =~ ${semver_pattern} ]]; then
    echo "Cargo package version is not valid SemVer: ${manifest_version}" >&2
    exit 1
fi

if [[ -n "${tag}" && "${tag}" != "v${manifest_version}" ]]; then
    echo "Release tag '${tag}' must exactly match package version 'v${manifest_version}'." >&2
    exit 1
fi

if [[ -n "${main_ref}" ]]; then
    if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        echo "Release ancestry validation requires a Git worktree." >&2
        exit 1
    fi
    if ! git check-ref-format "${main_ref}" >/dev/null 2>&1; then
        echo "Main branch reference is not a valid full Git reference: ${main_ref}" >&2
        exit 1
    fi
    if [[ "$(git rev-parse --is-shallow-repository)" == "true" ]]; then
        echo "Release ancestry validation requires complete Git history." >&2
        exit 1
    fi

    head_commit="$(git rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" \
        || {
            echo "The release commit cannot be resolved." >&2
            exit 1
        }
    main_commit="$(git rev-parse --verify "${main_ref}^{commit}" 2>/dev/null)" \
        || {
            echo "Main branch reference cannot be resolved: ${main_ref}" >&2
            exit 1
        }

    if ! git merge-base --is-ancestor "${head_commit}" "${main_commit}"; then
        echo "Release commit must be reachable from ${main_ref}." >&2
        exit 1
    fi

    if [[ -n "${tag}" ]]; then
        tag_commit="$(git rev-parse --verify "refs/tags/${tag}^{commit}" 2>/dev/null)" \
            || {
                echo "Release tag cannot be resolved: ${tag}" >&2
                exit 1
            }
        if [[ "${tag_commit}" != "${head_commit}" ]]; then
            echo "Release tag '${tag}' does not identify the checked-out commit." >&2
            exit 1
        fi
    fi
fi

printf '%s\n' "${manifest_version}"
