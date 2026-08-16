# syntax=docker/dockerfile:1.12.0@sha256:db1ff77fb637a5955317c7a3a62540196396d565f3dd5742e76dddbb6d75c4c5

FROM docker.io/library/rust:1.97.1-bullseye@sha256:90c2e6cd1f970487175cef2893e9429cb7bd3f20d344fe1941bb7dac6208b11f AS tools

ARG DEBIAN_FRONTEND=noninteractive

RUN rm -f /etc/apt/sources.list.d/debian.sources \
    && printf '%s\n' \
        'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20260803T000000Z bullseye main' \
        'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian-security/20260803T000000Z bullseye-security main' \
        'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20260803T000000Z bullseye-updates main' \
        > /etc/apt/sources.list \
    && apt-get update \
    && apt-get install --yes --no-install-recommends \
        binutils-arm-linux-gnueabihf \
        ca-certificates \
        curl \
        file \
        gcc-arm-linux-gnueabihf \
        jq \
        libc6-dev-armhf-cross \
        qemu-user-static \
    && rm -rf /var/lib/apt/lists/*

ARG CACHE_REVISION=none

RUN --mount=type=secret,id=host_ca \
    test -n "${CACHE_REVISION}"; \
    if [ -s /run/secrets/host_ca ]; then \
        if ! openssl x509 -inform DER -in /run/secrets/host_ca \
            -out /usr/local/share/ca-certificates/local-build-ca.crt; then \
            cp /run/secrets/host_ca /usr/local/share/ca-certificates/local-build-ca.crt; \
        fi; \
        update-ca-certificates; \
    fi

RUN rustup component add clippy rustfmt \
    && rustup target add armv7-unknown-linux-gnueabihf

RUN --mount=type=cache,id=rpi-health-release-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=rpi-health-release-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo install cargo-about --version 0.9.1 --locked --features cli

ENV CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
ENV CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUNNER="qemu-arm-static -L /usr/arm-linux-gnueabihf"
ENV CARGO_INCREMENTAL=0

FROM tools AS build-one

WORKDIR /workspace-one
COPY . .

ENV RUSTFLAGS="--remap-path-prefix=/workspace-one=. --remap-path-prefix=/usr/local/cargo=/cargo"

RUN --mount=type=cache,id=rpi-health-release-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=rpi-health-release-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo build --locked --release --target armv7-unknown-linux-gnueabihf \
    && install -D -m 0755 \
        target/armv7-unknown-linux-gnueabihf/release/rpi-health-mqtt \
        /out/rpi-health-mqtt

FROM tools AS build-two

WORKDIR /workspace-two
COPY . .

ENV RUSTFLAGS="--remap-path-prefix=/workspace-two=. --remap-path-prefix=/usr/local/cargo=/cargo"

RUN --mount=type=cache,id=rpi-health-release-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=rpi-health-release-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo build --locked --release --target armv7-unknown-linux-gnueabihf \
    && install -D -m 0755 \
        target/armv7-unknown-linux-gnueabihf/release/rpi-health-mqtt \
        /out/rpi-health-mqtt

FROM tools AS package

WORKDIR /workspace
COPY . .
COPY --from=build-one /out/rpi-health-mqtt /tmp/build-one/rpi-health-mqtt
COPY --from=build-two /out/rpi-health-mqtt /tmp/build-two/rpi-health-mqtt

RUN bash tests/release/run.sh \
    && cmp /tmp/build-one/rpi-health-mqtt /tmp/build-two/rpi-health-mqtt \
    && file /tmp/build-one/rpi-health-mqtt | grep -Eq 'ELF 32-bit LSB.*ARM' \
    && arm-linux-gnueabihf-readelf -h /tmp/build-one/rpi-health-mqtt \
        | grep -Eq 'Machine:[[:space:]]+ARM' \
    && arm-linux-gnueabihf-readelf -A /tmp/build-one/rpi-health-mqtt \
        | grep -Eq 'Tag_ABI_VFP_args: VFP registers' \
    && qemu-arm-static -L /usr/arm-linux-gnueabihf \
        /tmp/build-one/rpi-health-mqtt --version

ARG VERSION
ARG SOURCE_DATE_EPOCH=1

RUN chmod 0755 \
        scripts/generate-release-sbom.sh \
        scripts/package-release.sh \
        scripts/verify-release-archive.sh \
        scripts/verify-release-tag.sh \
    && release_version="${VERSION:-$(bash scripts/verify-release-tag.sh)}" \
    && archive_name="rpi-health-mqtt-${release_version}-armv7-unknown-linux-gnueabihf.tar.gz" \
    && scripts/package-release.sh \
        "${release_version}" \
        "${SOURCE_DATE_EPOCH}" \
        /tmp/build-one/rpi-health-mqtt \
        /tmp/package-one \
    && scripts/package-release.sh \
        "${release_version}" \
        "${SOURCE_DATE_EPOCH}" \
        /tmp/build-two/rpi-health-mqtt \
        /tmp/package-two \
    && cmp "/tmp/package-one/${archive_name}" "/tmp/package-two/${archive_name}" \
    && cmp "/tmp/package-one/${archive_name}.sha256" "/tmp/package-two/${archive_name}.sha256" \
    && scripts/verify-release-archive.sh \
        "/tmp/package-one/${archive_name}" \
        "${release_version}" \
    && scripts/verify-release-archive.sh \
        "/tmp/package-two/${archive_name}" \
        "${release_version}" \
    && mkdir -p /out \
    && install -m 0644 "/tmp/package-one/${archive_name}" "/out/${archive_name}" \
    && install -m 0644 "/tmp/package-one/${archive_name}.sha256" "/out/${archive_name}.sha256"

FROM package AS verify-sbom

ARG VERSION
ARG SOURCE_DATE_EPOCH=1

RUN set -eu; \
    release_version="${VERSION:-$(bash scripts/verify-release-tag.sh)}"; \
    archive_name="rpi-health-mqtt-${release_version}-armv7-unknown-linux-gnueabihf.tar.gz"; \
    archive="/out/${archive_name}"; \
    syft_version=1.42.3; \
    syft_sha256=0d6be741479eddd2c8644a288990c04f3df0d609bbc1599a005532a9dff63509; \
    syft_archive="syft_${syft_version}_linux_amd64.tar.gz"; \
    mkdir -p /tmp/sbom-input/release /tmp/syft; \
    (cd /out && sha256sum --check "${archive_name}.sha256"); \
    scripts/verify-release-archive.sh "${archive}" "${release_version}"; \
    cp Cargo.lock Cargo.toml /tmp/sbom-input/; \
    tar -xzf "${archive}" -C /tmp/sbom-input/release; \
    curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
        --output "/tmp/${syft_archive}" \
        "https://github.com/anchore/syft/releases/download/v${syft_version}/${syft_archive}"; \
    printf '%s  %s\n' "${syft_sha256}" "/tmp/${syft_archive}" \
        | sha256sum --check --strict; \
    tar -xzf "/tmp/${syft_archive}" -C /tmp/syft syft; \
    test "$(SYFT_CHECK_FOR_APP_UPDATE=false /tmp/syft/syft version -o json \
        | jq -r .version)" = "${syft_version}"; \
    scripts/generate-release-sbom.sh \
        /tmp/syft/syft \
        "${archive}" \
        "${release_version}" \
        "${SOURCE_DATE_EPOCH}" \
        /tmp/sbom-input \
        /tmp/sbom-one.spdx.json \
        example/example; \
    scripts/generate-release-sbom.sh \
        /tmp/syft/syft \
        "${archive}" \
        "${release_version}" \
        "${SOURCE_DATE_EPOCH}" \
        /tmp/sbom-input \
        /tmp/sbom-two.spdx.json \
        example/example; \
    cmp /tmp/sbom-one.spdx.json /tmp/sbom-two.spdx.json; \
    jq --exit-status '.packages | (type == "array" and length > 0)' \
        /tmp/sbom-one.spdx.json >/dev/null

FROM scratch AS export
COPY --from=package /out/ /
