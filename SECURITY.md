# Security policy

## Supported versions

Security fixes are provided for the latest released `0.1.x` version. The
unreleased default branch may change without compatibility guarantees.

| Version | Supported |
|---|:---:|
| Latest `0.1.x` release | Yes |
| Older releases | No |

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability and do not include
credentials, private hostnames, network addresses, broker exports, or production
telemetry in a report.

Use the repository's private GitHub Security Advisory reporting interface.
Include:

- the affected version or commit;
- the target architecture and operating system version;
- a concise impact description;
- minimal, synthetic reproduction steps;
- the expected and observed behavior; and
- any suggested mitigation.

Replace secrets and site-specific identifiers with obvious placeholders. If a
reproduction needs an MQTT credential, create a temporary account in an
isolated broker and report only synthetic values.

The maintainers will assess the report, coordinate a fix and disclosure as
appropriate, and publish supported-version guidance with a release. No response
or remediation timeline is guaranteed.

## Security boundaries

`rpi-health-mqtt` is a publisher-only monitoring service. It does not:

- subscribe to MQTT commands;
- expose an HTTP server or listening socket;
- administer broker users, ACLs, or bridges;
- run as root after installation;
- execute a shell; or
- store telemetry history locally.

The default MQTT endpoint is loopback and uses username/password
authentication. Version 0.1 does not configure TLS. Keep the broker local, or
provide a separately secured transport and network when using a non-loopback
endpoint.

The MQTT password is loaded from a separate restricted regular file. It must
never be committed, placed in TOML, passed on a command line, pasted into an
issue, or included in diagnostic output. The application's secret-bearing types
redact debug output, but operators must still protect journald, broker logs,
configuration backups, and process access.

The systemd unit intentionally omits `PrivateDevices` because Raspberry Pi
firmware tools may require `/dev/vcio`. The service instead runs as a dedicated
non-login user with `video` group access, no capabilities, and other filesystem
and kernel hardening.

## Dependency and release hygiene

Changes are expected to pass the repository's Docker validation gate, including
tests, Clippy, rustdoc, ARMv7 build verification, and an isolated authenticated
MQTT integration test. Release preparation also checks the locked dependency
set and generated artifacts. Review dependency changes and lockfile diffs
before merging.

Published releases provide an archive checksum, a checksummed SPDX JSON SBOM,
a third-party license report, and GitHub provenance/SBOM attestations. Verify
the checksum through a trusted release channel and verify provenance before
installing a downloaded binary. The exact procedure and trust limitations are
documented under [release artifacts](docs/installation-and-configuration.md#release-artifacts).
