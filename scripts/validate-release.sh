#!/usr/bin/env bash

set -euo pipefail

if [[ "$(docker info --format '{{.OSType}}')" != "linux" ]]; then
    echo "Docker must be running with the Linux engine." >&2
    exit 1
fi

mkdir -p .local
if [[ -n "${DOCKER_BUILD_CA_CERT:-}" ]]; then
    cp "${DOCKER_BUILD_CA_CERT}" .local/docker-ca.cer
elif [[ ! -f .local/docker-ca.cer ]]; then
    : > .local/docker-ca.cer
fi
cache_revision="$(sha256sum -- .local/docker-ca.cer | awk '{print $1}')"

docker buildx build \
    --file docker/release.Dockerfile \
    --target verify-sbom \
    --platform linux/amd64 \
    --secret id=host_ca,src=.local/docker-ca.cer \
    --build-arg "CACHE_REVISION=${cache_revision}" \
    --build-arg SOURCE_DATE_EPOCH=1 \
    --progress=plain \
    .
