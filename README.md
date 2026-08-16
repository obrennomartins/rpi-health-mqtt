# rpi-health-mqtt

`rpi-health-mqtt` is a small, native monitoring daemon for 32-bit Raspberry Pi
OS. It reads health data from Linux kernel interfaces and Raspberry Pi firmware,
publishes one compact JSON snapshot to a local MQTT broker, and registers a
single device with Home Assistant through MQTT Device Discovery.

The service is designed for a Raspberry Pi 2 Model B Rev 1.1 running the ARMv7
hard-float ABI. It does not receive commands, expose a network port, modify the
MQTT broker, or connect to a remote broker. An existing Mosquitto bridge may
forward its publications to another broker.

## Highlights

- CPU utilization, load, temperature, and frequency
- Memory, swap, root-filesystem usage, and uptime
- Current and since-boot undervoltage, throttling, frequency-cap, and thermal
  limit flags from `vcgencmd get_throttled`
- Partial-failure payloads: an unavailable measurement becomes `null` while
  useful measurements are still published
- One persistent authenticated MQTT connection with bounded queues and
  latest-state-only backpressure
- Retained Device Discovery and availability; non-retained periodic state
- Reconnection with exponential backoff and a fresh observation after every
  connection
- A non-root, hardened systemd unit and idempotent install/remove scripts
- Reproducible Docker validation on Linux amd64 and ARMv7 under QEMU

## Architecture

```text
rpi-health-mqtt
  |  reads /proc, /sys, statvfs, and vcgencmd
  |  publishes MQTT to 127.0.0.1:1883 by default
  v
local Mosquitto
  |  optional existing outbound bridge
  v
remote Mosquitto -> Home Assistant MQTT integration
```

The bridge and both brokers are external prerequisites. This project never
creates broker users, changes ACLs, or configures a bridge.

## Target and requirements

The production target is:

```text
armv7-unknown-linux-gnueabihf
```

Use Raspberry Pi OS 32-bit (`armhf`) on ARMv7 hardware. Do not use an AArch64
artifact. The runtime requires:

- Linux `/proc` and `/sys` interfaces;
- `vcgencmd` from the Raspberry Pi userland tools;
- permission for the service account to use the firmware interface, normally
  through membership in the `video` group;
- a local MQTT 3.1.1 broker with username/password authentication; and
- systemd for the provided service unit and installer.

The source build uses Rust 1.97.1. Docker with Buildx and Compose is required for
the complete development validation gate.

## Quick start

Build directly on a 32-bit Raspberry Pi:

```sh
cargo build --release --locked
sudo ./scripts/install.sh --binary target/release/rpi-health-mqtt
```

The first installation places an example at
`/etc/rpi-health-mqtt/config.toml`, but does not create an MQTT password. Edit
the configuration, create `/etc/rpi-health-mqtt/mqtt-password` with mode `0640`
and ownership `root:rpi-health-mqtt`, then verify the deployment:

```sh
sudo -u rpi-health-mqtt /usr/local/bin/rpi-health-mqtt \
  --config /etc/rpi-health-mqtt/config.toml check
sudo -u rpi-health-mqtt /usr/local/bin/rpi-health-mqtt \
  --config /etc/rpi-health-mqtt/config.toml print-once
sudo systemctl enable --now rpi-health-mqtt.service
```

The installer can be run again after the password is present; it preserves the
configuration and password and restarts the service. See the complete
[installation and configuration guide](docs/installation-and-configuration.md)
before deploying.

## Cross-build

From an x86_64 development host, `cross` provides the simplest Docker-backed
build:

```sh
cargo install cross --version 0.2.5 --locked
cross build --release --locked --target armv7-unknown-linux-gnueabihf
file target/armv7-unknown-linux-gnueabihf/release/rpi-health-mqtt
```

The result must be a 32-bit ARM ELF using the hard-float ABI. Install that
artifact with:

```sh
sudo ./scripts/install.sh
```

The installer's default artifact path is the cross-build path shown above.
`cross` is an optional convenience for local builds; the repository's pinned
Docker gate is the authoritative validation and release environment.

Every install, including a `DESTDIR` staging install, requires `file` and GNU
`readelf` from binutils. Before writing managed files, the installer accepts
only a little-endian ARMv7 EABI5 ELF32 executable that declares the hard-float
ABI and VFP register arguments. Staging skips live host, account, and systemd
operations; it does not bypass binary or managed-path validation.

## Configuration and credentials

Start from [config/config.example.toml](config/config.example.toml). All
identity values in the example are placeholders.

```toml
[device]
id = "example-pi"
name = "Example Raspberry Pi"

[mqtt]
host = "127.0.0.1"
port = 1883
client_id = "rpi-health-mqtt-example-pi"
username = "monitor-example"
password_file = "/etc/rpi-health-mqtt/mqtt-password"
base_topic = "example/monitor/example-pi"
discovery_prefix = "homeassistant"
keep_alive_seconds = 30

[collector]
interval_seconds = 30
root_filesystem = "/"
vcgencmd_path = "/usr/bin/vcgencmd"
command_timeout_seconds = 2
```

The MQTT password belongs only in the separate `password_file`; it is not a TOML
setting and is never included in Discovery or state payloads. On Unix, the
service rejects a password file that is group-writable or accessible by other
users. It removes one final LF or CRLF and preserves every other byte of a valid
UTF-8 password. The file may contain at most 65,536 bytes, while the credential
remaining after line-ending removal may contain at most 65,535 UTF-8 bytes. The
one-byte difference permits a maximum-length credential followed by LF.

Configuration is strict: unknown fields are rejected, paths must be absolute,
publication topics may not contain `+`, `#`, or empty levels, and identity
fields accept only their documented ASCII characters. Environment-variable
overrides are intentionally not supported. See the [configuration
reference](docs/installation-and-configuration.md#configuration-reference) for
limits and defaults.

## MQTT contract

For a configured device `example-pi`, the three topics are:

| Purpose | Topic | QoS | Retained |
|---|---|---:|:---:|
| State | `<base_topic>/state` | 0 | No |
| Availability | `<base_topic>/availability` | 1 | Yes |
| Device Discovery | `<discovery_prefix>/device/example-pi/config` | 1 | Yes |

Availability uses `online` and `offline`. The client registers retained
`offline` as its last will, publishes retained `online` after connecting, and
attempts to publish retained `offline` and receive its QoS 1 acknowledgement
during a graceful shutdown. Discovery is acknowledged before availability is
established; a newly collected, non-retained state follows. The same sequence
runs after every reconnection. `SIGTERM` and `SIGINT` use this bounded shutdown
path; an abrupt termination relies on the broker's last will.

Collection triggers and state delivery are deliberately coalescing. A periodic
tick is skipped when collection is still busy, with rate-limited warnings, and
bounded channels retain only the latest requested generation. A new connection
discards any disconnected observation and requests one fresh collection, so a
recovery cannot replay an old burst. Periodic history belongs in the receiving
system, not in this daemon.

All Home Assistant components use `expire_after = max(interval_seconds * 3,
90)`. This lets Home Assistant detect loss of the process, device, bridge, or
publication path even when retained availability cannot reflect a remote bridge
failure.

## Telemetry and health semantics

The version 1 state document contains:

- `observed_at` in UTC RFC 3339 and `uptime_seconds`;
- `cpu`: utilization, 1/5/15-minute load, temperature, and frequency;
- `memory` and `swap`: byte counts and utilization;
- `disk`: mount, total, used, unprivileged-available bytes, and utilization;
- `power`: raw firmware bitmask plus eight nullable Boolean flags;
- `health`: aggregate status and sanitized collection errors; and
- `service`: binary version and collection duration.

Unavailable values are explicit JSON `null`, not fabricated zeroes. Numeric
measurements are finite and rounded to a stable precision. Aggregate health is
evaluated in this order:

1. `critical` for a known current undervoltage, throttle, frequency cap, or
   soft temperature limit;
2. `degraded` when collection errors make the snapshot incomplete;
3. `warning` for a historical since-boot firmware flag; and
4. `ok` otherwise.

`vcgencmd get_throttled` uses these bits:

| Bit | Field | Meaning |
|---:|---|---|
| 0 | `undervoltage_now` | Undervoltage is present now |
| 1 | `arm_frequency_capped_now` | ARM frequency is capped now |
| 2 | `throttled_now` | CPU throttling is present now |
| 3 | `soft_temperature_limit_now` | Soft thermal limit is present now |
| 16 | `undervoltage_since_boot` | Undervoltage occurred since boot |
| 17 | `arm_frequency_capped_since_boot` | ARM frequency was capped since boot |
| 18 | `throttled_since_boot` | CPU throttling occurred since boot |
| 19 | `soft_temperature_limit_since_boot` | Soft thermal limit occurred since boot |

Historical flags indicate only that an event occurred since boot. They do not
say when it occurred or how many times it occurred. `measure_volts core` is not
a measurement of the board's 5 V input and is not collected.

## Home Assistant Discovery

One retained Device Discovery document defines 26 diagnostic entities under one
device. Twelve are enabled by default: health, temperature, CPU/memory/swap/disk
usage, disk available, uptime, current and historical undervoltage, and current
and historical throttling. The remaining load, frequency, byte-count,
observation, raw-flag, duration/error, frequency-cap, and soft-temperature-limit
entities can be enabled in Home Assistant.

Stable `unique_id` values prevent duplication across normal service, broker, or
Home Assistant restarts. The advertised `default_entity_id` is only an initial
suggestion; a user's later Home Assistant entity rename is not overwritten.
Optional Raspberry Pi model, serial, and revision metadata is discovered from
local kernel interfaces when available.

The full [entity list](docs/installation-and-configuration.md#home-assistant-entities)
documents all 26 components.

## Broker ACL and bridge prerequisites

At minimum, the local service identity needs write access to:

```text
<base_topic>/state
<base_topic>/availability
<discovery_prefix>/device/<device_id>/config
```

A conceptual Mosquitto ACL is:

```conf
user <service-mqtt-user>
topic write <base_topic>/#
topic write <discovery_prefix>/device/<device_id>/config
```

If a bridge is used, it must forward state, retained availability, and retained
Discovery outbound. The remote bridge identity must be allowed to write those
topics, and the Home Assistant broker identity must be allowed to read them.
Testing an unrelated topic does not prove that the Discovery prefix is covered
by the bridge and ACL rules.

## Commands and exit status

Omitting a subcommand starts the daemon:

```sh
rpi-health-mqtt --config /etc/rpi-health-mqtt/config.toml
rpi-health-mqtt --config /etc/rpi-health-mqtt/config.toml check
rpi-health-mqtt --config /etc/rpi-health-mqtt/config.toml check --skip-mqtt
rpi-health-mqtt --config /etc/rpi-health-mqtt/config.toml print-once
```

`check` validates the configuration and credential, local access and collector
prerequisites, firmware flags, runtime identity, architecture, and a short MQTT
connection probe without publishing telemetry. `--skip-mqtt` omits only that
broker probe; credential and local checks still run. `print-once` performs one
full collection, prints only pretty JSON to stdout, writes diagnostics to
stderr, and does not load credentials or connect to MQTT.

| Code | Meaning |
|---:|---|
| 0 | Success |
| 2 | Command-line usage error |
| 3 | Configuration-file error, or credential error while starting the daemon |
| 4 | A `check` prerequisite failed, including credential or MQTT failure |
| 5 | One-shot collection could not start or serialize |
| 6 | Unrecoverable daemon startup/runtime error |

Warnings and an explicitly skipped MQTT probe leave `check` at code 0. A
configuration file that cannot be loaded is rejected before diagnostics and
returns code 3.

Temporary broker or network failures do not terminate the daemon; they trigger
bounded reconnect backoff.

## Operations

Follow service health and resource use with:

```sh
systemctl status rpi-health-mqtt.service
journalctl -u rpi-health-mqtt.service -f
ps -o pid,pcpu,pmem,rss,vsz,etime,cmd -C rpi-health-mqtt
journalctl -u rpi-health-mqtt.service --since today
```

To upgrade, build or obtain a new ARMv7 hard-float binary and rerun
`scripts/install.sh --binary PATH`. Existing configuration and password files
are preserved. To remove the binary and service while preserving configuration,
run:

```sh
sudo ./scripts/uninstall.sh
```

Add `--purge-config` only when the managed configuration and password should
also be deleted. The service account is preserved in either case.

## Development validation

The complete validation gate runs format checks, tests, Clippy with warnings as
errors, rustdoc warnings as errors, an ARMv7 release build and ABI inspection,
QEMU execution, and authenticated MQTT integration tests against an isolated
Mosquitto container:

```powershell
pwsh -NoProfile -File scripts/validate-delivery.ps1
```

On a POSIX development host:

```sh
./scripts/validate-delivery.sh
```

Run the containerized workflow, dependency-policy, advisory, and secret scans
separately:

```sh
bash scripts/validate-ci.sh
```

The CI workflow also builds the complete release package when its validator is
present, allowing the CI delivery to run before release tooling is added:

```sh
if [[ -f scripts/validate-release.sh ]]; then
  bash scripts/validate-release.sh
fi
```

The release validator exercises deterministic ARMv7 builds, ABI inspection,
QEMU startup, release helper tests, packaging, and archive verification. It
does not create a tag, publish a release, or contact a production broker.

Docker must use the Linux engine and provide Buildx and Compose. Validation
uses only synthetic example identities and credentials.

Public Markdown can also be checked without network access:

```sh
bash scripts/validate-docs.sh
```

The built-in link, anchor, fence, entity-contract, and hygiene checks always
run. If `markdownlint-cli2` and `codespell` are installed, the script runs them
too. The Docker documentation stage supplies both tools, uses
[the repository Markdown
configuration](config/project.markdownlint-cli2.jsonc), and requires them.
`DOCS_REQUIRE_MARKDOWNLINT=1` and `DOCS_REQUIRE_CODESPELL=1` enable the same
fail-if-missing behavior in a custom environment.

## Release artifacts

A tagged release provides an ARMv7 hard-float archive, a detached archive
checksum, an SPDX JSON software bill of materials (SBOM), and `SHA256SUMS` for
the archive and SBOM. Tag releases also receive GitHub build-provenance and
SBOM attestations. Verify checksums and provenance before extracting or
installing an artifact; checksums prove integrity only when obtained through a
trusted channel.

The release archive includes the binary, installer, systemd unit,
configuration example, project license, and third-party license report. See
[release artifact verification](docs/installation-and-configuration.md#release-artifacts)
for exact commands and the relationship between checksums, the SBOM, and
attestations.

## Troubleshooting and limitations

The detailed guide covers configuration permissions, MQTT authentication and
ACL failures, missing Discovery across a bridge, `vcgencmd` access, unavailable
sysfs metrics, and wrong-architecture binaries.

Version 0.1 deliberately has no remote commands, HTTP server, local history,
Prometheus endpoint, per-process metrics, broker provisioning, bridge changes,
automatic updates, or TLS configuration. Plain MQTT is safe only because the
default broker endpoint is local loopback; secure any non-loopback deployment at
the transport/network layer.

Docker release builds link against Debian Bullseye's glibc 2.31. QEMU uses
matching container libraries, so it does not prove compatibility with an older
target userspace. Check `ldd --version` on the Raspberry Pi; an older system may
require a native build or a compatible sysroot.

Docker and QEMU validate the software, ARM instruction set, and hard-float ABI,
but final acceptance also requires tests on the real Raspberry Pi: loader and
library compatibility, native execution, `/dev/vcio` access under the service
account, systemd hardening compatibility, end-to-end bridge/Discovery behavior,
CPU and RSS measurement, and a 24-hour stability run. Follow the [deployment
validation checklist](docs/installation-and-configuration.md#deployment-validation-checklist).

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow. Report
suspected vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

## License

Licensed under the [MIT License](LICENSE).
