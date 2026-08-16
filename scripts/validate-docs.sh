#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd -- "${repository_root}"

documents=(
    README.md
    docs/installation-and-configuration.md
    CONTRIBUTING.md
    SECURITY.md
)

required_commands=(awk cmp cut diff dirname grep mktemp realpath rm sed sort tail tr uniq wc)
for command_name in "${required_commands[@]}"; do
    if ! command -v "${command_name}" >/dev/null 2>&1; then
        printf 'Documentation validation requires %s.\n' "${command_name}" >&2
        exit 2
    fi
done

failure_count=0
fail() {
    printf 'Documentation validation failed: %s\n' "$*" >&2
    failure_count=$((failure_count + 1))
}

temporary_directory="$(mktemp -d)"
cleanup() {
    rm -rf -- "${temporary_directory}"
}
trap cleanup EXIT

require_external_linters="${DOCS_REQUIRE_EXTERNAL_LINTERS:-0}"
require_markdownlint="${DOCS_REQUIRE_MARKDOWNLINT:-${require_external_linters}}"
require_codespell="${DOCS_REQUIRE_CODESPELL:-${require_external_linters}}"
for requirement in \
    "DOCS_REQUIRE_EXTERNAL_LINTERS=${require_external_linters}" \
    "DOCS_REQUIRE_MARKDOWNLINT=${require_markdownlint}" \
    "DOCS_REQUIRE_CODESPELL=${require_codespell}"; do
    case "${requirement#*=}" in
        0|1)
            ;;
        *)
            printf '%s must be 0 or 1.\n' "${requirement%%=*}" >&2
            exit 2
            ;;
    esac
done
if [[ "${require_markdownlint}" == 1 ]] \
    && ! command -v markdownlint-cli2 >/dev/null 2>&1; then
    printf 'markdownlint-cli2 is required when DOCS_REQUIRE_MARKDOWNLINT=1.\n' >&2
    exit 2
fi
if [[ "${require_codespell}" == 1 ]] \
    && ! command -v codespell >/dev/null 2>&1; then
    printf 'codespell is required when DOCS_REQUIRE_CODESPELL=1.\n' >&2
    exit 2
fi

for document in "${documents[@]}"; do
    if [[ ! -f "${document}" ]]; then
        fail "missing public document: ${document}"
        continue
    fi

    if [[ "$(grep -c '^# ' "${document}" || true)" -ne 1 ]]; then
        fail "${document} must contain exactly one level-one heading"
    fi
    if LC_ALL=C grep -n $'\r' "${document}" >/dev/null; then
        fail "${document} contains carriage returns; use LF line endings"
    fi
    if LC_ALL=C grep -nE '[[:blank:]]+$' "${document}" >/dev/null; then
        fail "${document} contains trailing whitespace"
    fi
    if LC_ALL=C grep -n $'\t' "${document}" >/dev/null; then
        fail "${document} contains tab characters"
    fi
    if [[ -s "${document}" ]] && [[ "$(tail -c 1 "${document}" | wc -l)" -ne 1 ]]; then
        fail "${document} must end with a newline"
    fi

    open_fence_character=''
    open_fence_length=0
    open_fence_line=0
    line_number=0
    fence_pattern='^(`{3,}|~{3,})'
    while IFS= read -r line || [[ -n "${line}" ]]; do
        line_number=$((line_number + 1))
        trimmed="${line#"${line%%[![:space:]]*}"}"
        if [[ "${trimmed}" =~ ${fence_pattern} ]]; then
            marker="${BASH_REMATCH[1]}"
            marker_character="${marker:0:1}"
            if [[ -z "${open_fence_character}" ]]; then
                open_fence_character="${marker_character}"
                open_fence_length=${#marker}
                open_fence_line=${line_number}
            elif [[ "${marker_character}" == "${open_fence_character}" ]] \
                && (( ${#marker} >= open_fence_length )) \
                && [[ "${trimmed:${#marker}}" =~ ^[[:space:]]*$ ]]; then
                open_fence_character=''
                open_fence_length=0
                open_fence_line=0
            fi
        fi
    done < "${document}"
    if [[ -n "${open_fence_character}" ]]; then
        fail "${document} has an unclosed code fence from line ${open_fence_line}"
    fi
done

normalize_heading() {
    LC_ALL=C tr '[:upper:]' '[:lower:]' \
        | sed -E \
            -e 's/<[^>]*>//g' \
            -e 's/`//g' \
            -e 's/[^a-z0-9 _-]//g' \
            -e 's/[[:space:]]+/-/g' \
            -e 's/^-+//' \
            -e 's/-+$//'
}

anchor_exists() {
    local document=$1
    local expected_anchor=$2
    local heading heading_text anchor

    expected_anchor="$(printf '%s' "${expected_anchor}" | normalize_heading)"
    while IFS= read -r heading; do
        heading_text="$(
            printf '%s\n' "${heading}" \
                | sed -E \
                    -e 's/^#{1,6}[[:space:]]+//' \
                    -e 's/[[:space:]]+#+[[:space:]]*$//'
        )"
        anchor="$(printf '%s' "${heading_text}" | normalize_heading)"
        if [[ "${anchor}" == "${expected_anchor}" ]]; then
            return 0
        fi
    done < <(grep -E '^#{1,6}[[:space:]]+' "${document}" || true)
    return 1
}

for document in "${documents[@]}"; do
    [[ -f "${document}" ]] || continue
    while IFS= read -r token; do
        link="${token#](}"
        link="${link%)}"
        case "${link}" in
            http://*|https://*|mailto:*|ftp://*)
                continue
                ;;
        esac

        target_path="${link%%#*}"
        if [[ "${link}" == *'#'* ]]; then
            fragment="${link#*#}"
        else
            fragment=''
        fi
        if [[ -z "${target_path}" ]]; then
            target_document="${document}"
        else
            target_document="$(dirname -- "${document}")/${target_path}"
        fi

        if [[ ! -f "${target_document}" ]]; then
            fail "${document} links to missing file: ${target_path}"
            continue
        fi
        resolved_target="$(realpath -- "${target_document}")"
        case "${resolved_target}" in
            "${repository_root}"/*)
                ;;
            *)
                fail "${document} links outside the repository: ${target_path}"
                continue
                ;;
        esac
        if [[ -n "${fragment}" ]] && ! anchor_exists "${target_document}" "${fragment}"; then
            fail "${document} links to missing anchor #${fragment} in ${target_path:-${document}}"
        fi
    done < <(grep -oE '\]\([^)]*\)' "${document}" || true)
done

entity_table="docs/installation-and-configuration.md"
documented_entities="${temporary_directory}/documented-entities"
source_entities="${temporary_directory}/source-entities"

awk '
    /^## Home Assistant entities[[:space:]]*$/ {
        in_table = 1
        next
    }
    in_table && /^##[[:space:]]/ {
        exit
    }
    in_table && /^\|[[:space:]]*`/ {
        key = $2
        name = $4
        gsub(/^[[:space:]]*`|`[[:space:]]*$/, "", key)
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", name)
        print key "|" name
    }
' FS='|' "${entity_table}" | LC_ALL=C sort > "${documented_entities}"

awk '
    function quoted_value(line, content, closing) {
        sub(/^[^"]*"/, "", line)
        closing = index(line, "\"")
        return substr(line, 1, closing - 1)
    }
    /^[[:space:]]*(sensor|binary_sensor)\([[:space:]]*$/ {
        getline
        key = quoted_value($0)
        getline
        name = quoted_value($0)
        print key "|" name
    }
' src/discovery.rs | LC_ALL=C sort > "${source_entities}"

documented_entity_count="$(wc -l < "${documented_entities}")"
if [[ "${documented_entity_count}" -ne 26 ]]; then
    fail "the Home Assistant entity table must contain 26 rows, found ${documented_entity_count}"
fi
duplicate_entity_keys="$(cut -d '|' -f 1 "${documented_entities}" | uniq -d)"
if [[ -n "${duplicate_entity_keys}" ]]; then
    fail "the Home Assistant entity table contains duplicate keys: ${duplicate_entity_keys}"
fi
if ! cmp -s "${documented_entities}" "${source_entities}"; then
    diff -u "${source_entities}" "${documented_entities}" >&2 || true
    fail "the documented Home Assistant keys or English names differ from src/discovery.rs"
fi

if LC_ALL=C grep -nE \
    '([A-Za-z]:\\Users\\|/Users/[^/[:space:]]+|/home/[^/[:space:]]+)' \
    "${documents[@]}" >/dev/null; then
    fail "public documentation contains a user-profile path"
fi

if command -v markdownlint-cli2 >/dev/null 2>&1; then
    if ! markdownlint-cli2 \
        --config config/project.markdownlint-cli2.jsonc \
        "${documents[@]}"; then
        fail "markdownlint-cli2 reported a Markdown style error"
    fi
else
    printf 'Optional Markdown style check skipped: markdownlint-cli2 is not installed.\n'
fi

if command -v codespell >/dev/null 2>&1; then
    if ! codespell --quiet-level 2 "${documents[@]}"; then
        fail "codespell reported a spelling error"
    fi
else
    printf 'Optional spelling check skipped: codespell is not installed.\n'
fi

if (( failure_count != 0 )); then
    printf 'Documentation validation found %d error(s).\n' "${failure_count}" >&2
    exit 1
fi

printf 'Documentation validation passed for %d files and 26 Home Assistant entities.\n' \
    "${#documents[@]}"
