# syntax=docker/dockerfile:1.12

FROM docker.io/library/eclipse-mosquitto:2.0.22@sha256:212f89e1eaeb2c322d6441b64396e3346026674db8fa9c27beac293405c32b3c AS broker-validation

COPY --chown=mosquitto:mosquitto --chmod=0600 docker/mosquitto/ /mosquitto/config/

USER mosquitto
ENTRYPOINT ["mosquitto"]
CMD ["-c", "/mosquitto/config/mosquitto.conf"]

FROM docker.io/library/rust:1.97.1-bullseye@sha256:90c2e6cd1f970487175cef2893e9429cb7bd3f20d344fe1941bb7dac6208b11f AS toolchain

ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        binutils-arm-linux-gnueabihf \
        ca-certificates \
        file \
        gcc-arm-linux-gnueabihf \
        libc6-dev-armhf-cross \
        qemu-user-static \
        shellcheck \
        systemd \
    && rm -rf /var/lib/apt/lists/*

RUN --mount=type=secret,id=host_ca \
    if [ -s /run/secrets/host_ca ]; then \
        if ! openssl x509 -inform DER -in /run/secrets/host_ca \
            -out /usr/local/share/ca-certificates/local-build-ca.crt; then \
            cp /run/secrets/host_ca /usr/local/share/ca-certificates/local-build-ca.crt; \
        fi; \
        update-ca-certificates; \
    fi

RUN rustup component add clippy rustfmt \
    && rustup target add armv7-unknown-linux-gnueabihf

ENV CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
ENV CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUNNER="qemu-arm-static -L /usr/arm-linux-gnueabihf"

WORKDIR /workspace
COPY . .

FROM toolchain AS verify-amd64
COPY . .
RUN --mount=type=cache,id=rpi-health-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=rpi-health-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=rpi-health-target-amd64,target=/workspace/target \
    bash scripts/validate-in-container.sh amd64

FROM toolchain AS verify-armv7
COPY . .
RUN --mount=type=cache,id=rpi-health-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=rpi-health-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=rpi-health-target-armv7,target=/workspace/target \
    bash scripts/validate-in-container.sh armv7

FROM toolchain AS integration
COPY . .
RUN --mount=type=cache,id=rpi-health-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=rpi-health-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo test --locked --test mqtt_integration --no-run \
    && find target/debug/deps -maxdepth 1 -type f -name 'mqtt_integration-*' -perm /111 \
       -exec cp '{}' /usr/local/bin/mqtt-integration-test ';'
ENTRYPOINT ["/usr/local/bin/mqtt-integration-test"]
CMD ["--ignored", "--test-threads=1"]
