#!/usr/bin/env bash

set -euo pipefail

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "CI validation requires a Git worktree." >&2
    exit 1
fi

git config --global --add safe.directory "$(pwd)"

if git ls-files --error-unmatch -- HANDOFF.md >/dev/null 2>&1; then
    echo "HANDOFF.md must not be tracked by Git." >&2
    exit 1
fi

environment_pathspecs=(':(glob).env*' ':(glob)**/.env*')
if [[ -n "$(git ls-files -- "${environment_pathspecs[@]}")" ]]; then
    echo ".env files must not be tracked by Git." >&2
    exit 1
fi
if [[ -n "$(git log --all --format= --name-only -- "${environment_pathspecs[@]}" \
    | sed '/^$/d')" ]]; then
    echo ".env files must not appear in Git history." >&2
    exit 1
fi

actionlint -color
cargo-audit audit --file Cargo.lock --deny warnings
cargo-deny --all-features --locked check

temporary_directory="$(mktemp -d)"
cleanup() {
    rm -rf -- "${temporary_directory}"
}
trap cleanup EXIT

index_directory="${temporary_directory}/index"
mkdir -p "${index_directory}"
git checkout-index --all --prefix="${index_directory}/"

gitleaks git \
    --no-banner \
    --redact \
    --log-opts="--all" \
    .
gitleaks dir \
    --no-banner \
    --redact \
    "${index_directory}"
