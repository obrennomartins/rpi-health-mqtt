#!/usr/bin/env bash

set -euo pipefail

if [[ "$(docker info --format '{{.OSType}}')" != "linux" ]]; then
  echo "Docker must be running with the Linux engine." >&2
  exit 1
fi

mkdir -p .local
if [[ -n "${DOCKER_BUILD_CA_CERT:-}" ]]; then
  cp "${DOCKER_BUILD_CA_CERT}" .local/docker-ca.cer
elif [[ ! -f ".local/docker-ca.cer" ]]; then
  : > .local/docker-ca.cer
fi

docker buildx bake verify --progress=plain

compose_file="docker/compose.validation.yml"
if [[ -f "${compose_file}" ]]; then
  cleanup() {
    docker compose --file "${compose_file}" down --volumes --remove-orphans
  }
  trap cleanup EXIT
  docker compose --file "${compose_file}" up --build --abort-on-container-exit --exit-code-from integration
fi
