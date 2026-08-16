# Contributing

Contributions are welcome when they preserve the project's small, secure, and
predictable operating model.

## Project conventions

- Write source code, Rust documentation, comments, tests, commit messages, and
  public documentation in English.
- Never commit passwords, tokens, private keys, personal paths, private
  hostnames, production telemetry, or site-specific broker configuration.
- Use synthetic identities and credentials in examples and tests.
- Keep the service publisher-only. New command subscriptions, listening
  services, remote control, broker administration, and unrelated monitoring
  scope require a separate design discussion.
- Keep the ARMv7 32-bit hard-float target working. Do not assume ARM64 or a
  64-bit `usize`.
- Prefer direct `/proc`, `/sys`, and small system interfaces over heavyweight
  monitoring dependencies.
- Preserve partial collection: one unavailable metric must not discard useful
  observations.
- Preserve stable state-schema fields and Home Assistant unique IDs. Breaking
  wire changes require an explicit schema/version design.
- Do not add `unsafe` code; the package forbids it.

## Development environment

Install:

- Rust 1.97.1, as pinned by `rust-toolchain.toml`;
- Docker using the Linux engine;
- Docker Buildx; and
- Docker Compose.

The Docker gate supplies the Linux, ARMv7 cross-compilation, QEMU, and Mosquitto
test dependencies. It does not use production broker configuration.

## Making a change

1. Create a focused branch.
2. Add or update tests with the implementation.
3. Document public Rust items using normal rustdoc conventions. Missing
   documentation is denied.
4. Update the README or installation guide when behavior, configuration,
   deployment, or the MQTT contract changes.
5. Run the deterministic public-documentation checks.
6. Run the complete Docker validation gate.
7. Inspect the complete diff for sensitive or machine-specific information.

Use small, imperative commit messages with the established conventional prefix,
for example:

```text
feat: add a bounded collection trigger
fix: preserve unknown power flags
docs: clarify bridge ACL requirements
test: cover broker restart recovery
```

One commit should represent one independently understandable delivery. Avoid
mixing formatting-only or unrelated changes into functional commits.

## Validation

Validate local Markdown links and anchors, balanced fences, the 26-row entity
table, and basic file hygiene without network access:

```sh
bash scripts/validate-docs.sh
```

The script also invokes `markdownlint-cli2` and `codespell` when available.
The Docker documentation stage supplies and requires both tools. In a custom
environment, set `DOCS_REQUIRE_MARKDOWNLINT=1` or
`DOCS_REQUIRE_CODESPELL=1` to make a missing tool an error. No documentation
check needs network access at runtime.

On Windows PowerShell:

```powershell
pwsh -NoProfile -File scripts/validate-delivery.ps1
```

On a POSIX host:

```sh
./scripts/validate-delivery.sh
```

Run workflow linting, dependency policy/advisory checks, and redacted secret
scans in the pinned CI tools image:

```sh
bash scripts/validate-ci.sh
```

CI also runs the release package gate when that delivery is present:

```sh
if [[ -f scripts/validate-release.sh ]]; then
  bash scripts/validate-release.sh
fi
```

The conditional preserves incremental delivery validation. In a complete
checkout, the release gate must pass; it builds and verifies the package but
does not create a tag or publish externally.

The gate runs:

- `cargo fmt --check`;
- all-target, all-feature tests;
- Clippy with warnings denied;
- rustdoc with warnings denied;
- tests and a release build for
  `armv7-unknown-linux-gnueabihf`;
- ELF and hard-float ABI inspection;
- binary execution under QEMU; and
- the authenticated MQTT integration lifecycle in Docker Compose.

Do not bypass a failed gate by weakening a lint, test, broker ACL, or
architecture assertion without documenting the technical reason.

For a small local edit, the following commands provide quick feedback, but they
do not replace the Docker gate:

```sh
cargo fmt --all -- --check
cargo test --all-targets --all-features --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked
```

## Tests

Keep parser tests deterministic and use fixtures under `tests/fixtures` for
kernel/firmware samples. Use injected filesystem, clock, command-runner, and
protocol state where possible. Tests must cover malformed and unavailable input,
large counters on 32-bit targets, timeout behavior, nullable serialization, and
secret-safe failures.

MQTT integration tests use only the isolated broker defined in
`docker/compose.validation.yml`. Do not point tests at a developer or
production broker.

Installer tests must use the isolated `DESTDIR` path and fake systemctl
provided under `tests/install`. Never run destructive install/remove tests
against the host filesystem.

`DESTDIR` is not a binary-validation bypass. Staging still requires `file` and
GNU `readelf` and accepts only the ARMv7 EABI5 hard-float/VFP artifact; only
live kernel/userland, account, and systemd operations are skipped.

## Pull requests

A pull request should explain:

- the operational problem and chosen behavior;
- wire-contract or configuration effects;
- resource, security, and ARMv7 implications;
- tests added or changed; and
- any real Raspberry Pi verification still required.

Before submitting, verify that:

- every tracked file is intended to be public;
- no local-only handoff, generated artifact, credential, or private identifier
  is staged;
- `Cargo.lock` matches dependency changes;
- the Docker gate passes; and
- user-facing documentation matches the implementation.

## Hardware validation

QEMU verifies architecture compatibility but not Raspberry Pi firmware or
systemd behavior. The repository release image links against Debian Bullseye's
glibc 2.31, and QEMU runs with matching container libraries; it also does not
prove compatibility with an older target userspace. Changes affecting
collection, installation, shutdown, loader compatibility, or resource use
should follow the real-device checklist in
[the installation guide](docs/installation-and-configuration.md#deployment-validation-checklist).

Report hardware results with sanitized identifiers. Never include a broker
password or other production secret.

## License

By contributing, you agree that your contribution is licensed under the
project's [MIT License](LICENSE).
