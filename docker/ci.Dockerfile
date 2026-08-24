# syntax=docker/dockerfile:1.12.0@sha256:db1ff77fb637a5955317c7a3a62540196396d565f3dd5742e76dddbb6d75c4c5

FROM docker.io/library/rust:1.98.0-bullseye@sha256:97cc99038824c3cee60ae9d0f75e0171ae0ae8b80786d26d0e7d21956f8d0164

ARG DEBIAN_FRONTEND=noninteractive

RUN rm -f /etc/apt/sources.list.d/debian.sources \
    && printf '%s\n' \
        'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20260803T000000Z bullseye main' \
        'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian-security/20260803T000000Z bullseye-security main' \
        'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20260803T000000Z bullseye-updates main' \
        > /etc/apt/sources.list \
    && apt-get update \
    && apt-get install --yes --no-install-recommends \
        ca-certificates \
        curl \
        git \
        openssl \
        shellcheck \
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

RUN set -eu; \
    actionlint_version=1.7.12; \
    actionlint_sha256=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8; \
    cargo_audit_version=0.22.2; \
    cargo_audit_sha256=7fb9497f8594b389e5fce5ef9b92db08432996895b2e0c5a0167a69ed445c428; \
    cargo_deny_version=0.20.2; \
    cargo_deny_sha256=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f; \
    gitleaks_version=8.30.1; \
    gitleaks_sha256=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb; \
    mkdir -p /tmp/tools/actionlint /tmp/tools/cargo-audit /tmp/tools/cargo-deny /tmp/tools/gitleaks; \
    actionlint_archive="actionlint_${actionlint_version}_linux_amd64.tar.gz"; \
    cargo_audit_archive="cargo-audit-x86_64-unknown-linux-musl-v${cargo_audit_version}.tgz"; \
    cargo_deny_archive="cargo-deny-${cargo_deny_version}-x86_64-unknown-linux-musl.tar.gz"; \
    gitleaks_archive="gitleaks_${gitleaks_version}_linux_x64.tar.gz"; \
    curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
        --output "/tmp/tools/${actionlint_archive}" \
        "https://github.com/rhysd/actionlint/releases/download/v${actionlint_version}/${actionlint_archive}"; \
    curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
        --output "/tmp/tools/${cargo_audit_archive}" \
        "https://github.com/rustsec/rustsec/releases/download/cargo-audit/v${cargo_audit_version}/${cargo_audit_archive}"; \
    curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
        --output "/tmp/tools/${cargo_deny_archive}" \
        "https://github.com/EmbarkStudios/cargo-deny/releases/download/${cargo_deny_version}/${cargo_deny_archive}"; \
    curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
        --output "/tmp/tools/${gitleaks_archive}" \
        "https://github.com/gitleaks/gitleaks/releases/download/v${gitleaks_version}/${gitleaks_archive}"; \
    printf '%s  %s\n' \
        "${actionlint_sha256}" "/tmp/tools/${actionlint_archive}" \
        "${cargo_audit_sha256}" "/tmp/tools/${cargo_audit_archive}" \
        "${cargo_deny_sha256}" "/tmp/tools/${cargo_deny_archive}" \
        "${gitleaks_sha256}" "/tmp/tools/${gitleaks_archive}" \
        | sha256sum --check --strict; \
    tar -xzf "/tmp/tools/${actionlint_archive}" -C /tmp/tools/actionlint actionlint; \
    tar -xzf "/tmp/tools/${cargo_audit_archive}" --strip-components=1 -C /tmp/tools/cargo-audit; \
    tar -xzf "/tmp/tools/${cargo_deny_archive}" --strip-components=1 -C /tmp/tools/cargo-deny; \
    tar -xzf "/tmp/tools/${gitleaks_archive}" -C /tmp/tools/gitleaks gitleaks; \
    install -m 0755 /tmp/tools/actionlint/actionlint /usr/local/bin/actionlint; \
    install -m 0755 /tmp/tools/cargo-audit/cargo-audit /usr/local/bin/cargo-audit; \
    install -m 0755 /tmp/tools/cargo-deny/cargo-deny /usr/local/bin/cargo-deny; \
    install -m 0755 /tmp/tools/gitleaks/gitleaks /usr/local/bin/gitleaks; \
    test "$(actionlint -version | sed -n '1p')" = "${actionlint_version}"; \
    test "$(cargo-audit --version)" = "cargo-audit ${cargo_audit_version}"; \
    test "$(cargo-deny --version)" = "cargo-deny ${cargo_deny_version}"; \
    test "$(gitleaks version)" = "${gitleaks_version}"; \
    rm -rf /tmp/tools

WORKDIR /repo

ENTRYPOINT ["bash", "scripts/validate-ci-in-container.sh"]
