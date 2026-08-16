# Installation and configuration

This guide covers a production deployment of `rpi-health-mqtt` on Raspberry Pi
OS 32-bit with a local Mosquitto broker. Replace every example identity and
topic with values appropriate for the target device. Never copy a real password
into this repository, a command line, an issue, or a log.

## Contents

- [Deployment model](#deployment-model)
- [Prerequisites](#prerequisites)
- [Build](#build)
- [Install](#install)
- [Configuration reference](#configuration-reference)
- [Credential file](#credential-file)
- [Broker ACLs and bridge](#broker-acls-and-bridge)
- [Preflight diagnostics](#preflight-diagnostics)
- [Start and operate the service](#start-and-operate-the-service)
- [MQTT publications](#mqtt-publications)
- [State payload](#state-payload)
- [Home Assistant entities](#home-assistant-entities)
- [Release artifacts](#release-artifacts)
- [Upgrade](#upgrade)
- [Uninstall](#uninstall)
- [Troubleshooting](#troubleshooting)
- [Deployment validation checklist](#deployment-validation-checklist)

## Deployment model

The daemon publishes only to the broker configured in `mqtt.host`, which
defaults to loopback:

```text
Raspberry Pi kernel and firmware
            |
            v
    rpi-health-mqtt
            |
            v
local Mosquitto at 127.0.0.1:1883
            |
            | optional pre-existing outbound bridge
            v
remote Mosquitto and Home Assistant
```

The application neither installs nor administers Mosquitto. Create the service
MQTT account, ACLs, and any bridge through the normal broker administration
process before starting the daemon. The application does not need and should
not receive credentials for a remote broker.

## Prerequisites

The primary production target is a Raspberry Pi 2 Model B Rev 1.1 on ARMv7
Raspberry Pi OS 32-bit (`armhf`). Confirm the target before installing:

```sh
uname -m
dpkg --print-architecture
```

Expected values are typically `armv7l` and `armhf`. The installed executable
must target `armv7-unknown-linux-gnueabihf`, not AArch64.

The target also needs:

- systemd;
- a local, authenticated MQTT broker;
- Raspberry Pi userland tools providing `vcgencmd`;
- a `video` group that can access the firmware interface; and
- standard Linux `/proc` and `/sys` mounts.

Verify the firmware command before deployment:

```sh
/usr/bin/vcgencmd get_throttled
/usr/bin/vcgencmd measure_temp
```

The first command is required for every observation. The second is used only as
a temperature fallback when the thermal sysfs file is absent.

## Build

### Native ARMv7 build

With Rust 1.97.1 installed on the Raspberry Pi:

```sh
cargo build --release --locked
file target/release/rpi-health-mqtt
```

The file description must identify a 32-bit ARM ELF. Pass this native artifact
explicitly to the installer:

```sh
sudo ./scripts/install.sh --binary target/release/rpi-health-mqtt
```

### Docker-backed cross-build

On a non-ARM development host, install `cross` and build the pinned target:

```sh
cargo install cross --version 0.2.5 --locked
cross build --release --locked --target armv7-unknown-linux-gnueabihf
file target/armv7-unknown-linux-gnueabihf/release/rpi-health-mqtt
```

The artifact must be a 32-bit ARM ELF using the hard-float ABI:

```text
target/armv7-unknown-linux-gnueabihf/release/rpi-health-mqtt
```

That location is the installer's default, so no `--binary` option is required
for a cross-build. `cross` is an optional local convenience; the repository's
pinned Docker gate remains the authoritative validation and release
environment.

The repository's release container links against Debian Bullseye and glibc
2.31. QEMU runs the executable with matching ARM libraries inside that
container; it does not prove that the executable will load on an older
Raspberry Pi OS userspace. Check the target before deployment:

```sh
ldd --version
```

If the target provides an older glibc, build natively on that Raspberry Pi or
use a cross-compilation sysroot compatible with the target operating system.

### Repository validation

Before deploying a changed build, run the complete Docker gate from the
repository root:

```powershell
pwsh -NoProfile -File scripts/validate-delivery.ps1
```

or:

```sh
./scripts/validate-delivery.sh
```

In a TLS-inspecting environment, set `DOCKER_BUILD_CA_CERT` to a trusted CA
certificate in DER or PEM format before running a Docker gate. The validation
scripts copy it only to the ignored `.local/docker-ca.cer` path and mount it as
a BuildKit secret. Never commit that certificate.

The gate checks formatting, all targets and features, Clippy, rustdoc, an ARMv7
release artifact and hard-float ABI, QEMU execution, and the authenticated MQTT
lifecycle against an isolated Mosquitto container. These checks validate the
build and protocol behavior, not Raspberry Pi firmware access, target glibc
compatibility, systemd behavior, or physical resource use.

Run the CI and supply-chain checks in their pinned Docker tool image as a
separate gate:

```sh
bash scripts/validate-ci.sh
```

That command runs workflow/ShellCheck validation, RustSec and dependency-policy
checks, and redacted secret scans over both Git history and the checked-out
files. The repository CI conditionally runs release-package validation when the
release tooling is present:

```sh
if [[ -f scripts/validate-release.sh ]]; then
  bash scripts/validate-release.sh
fi
```

The release validator builds the Docker `package` target, including two
byte-identical ARMv7 builds, ABI and QEMU checks, release helper tests,
packaging, and archive verification. It does not create or publish a release.

## Install

The installer must run as root. It is idempotent and refuses a live install on
a non-ARMv7/armhf system. Its optional `DESTDIR` staging mode is intended for
packaging and automated tests, not production.

Both live and staging installs require `file` and GNU `readelf` from binutils.
The supplied executable is accepted only when its file and ELF attributes show
a little-endian ARMv7 EABI5 ELF32 executable, the ARM hard-float ABI, and VFP
register arguments. Staging skips only live kernel/userland validation, service
account management, and systemd operations. It still requires root, validates
the binary, rejects unsafe managed paths, and applies the staged file modes.
Because no service account is resolved in staging, configuration paths are
owned by `root:root` there.

```sh
sudo ./scripts/install.sh
```

For a binary at another path:

```sh
sudo ./scripts/install.sh --binary /absolute/path/to/rpi-health-mqtt
```

The installer:

1. creates the `rpi-health-mqtt` system group and non-login system user when
   needed;
2. adds that user to `video`;
3. installs the executable at `/usr/local/bin/rpi-health-mqtt`;
4. installs the systemd unit at
   `/etc/systemd/system/rpi-health-mqtt.service`;
5. creates `/etc/rpi-health-mqtt` with restricted permissions;
6. installs the example as `/etc/rpi-health-mqtt/config.toml` only when no
   configuration exists;
7. preserves an existing configuration and password; and
8. enables and restarts the service only when both configuration and password
   files exist and are non-empty.

Expected live-install permissions are:

| Path | Owner | Mode |
|---|---|---:|
| `/usr/local/bin/rpi-health-mqtt` | `root:root` | `0755` |
| `/etc/rpi-health-mqtt` | `root:rpi-health-mqtt` | `0750` |
| `/etc/rpi-health-mqtt/config.toml` | `root:rpi-health-mqtt` | `0640` |
| `/etc/rpi-health-mqtt/mqtt-password` | `root:rpi-health-mqtt` | `0640` |
| systemd unit | `root:root` | `0644` |

The service may remain stopped after the first installation because the
installer intentionally does not invent a credential.

## Configuration reference

Edit `/etc/rpi-health-mqtt/config.toml`. The complete public example is
[config.example.toml](../config/config.example.toml).

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

Unknown sections and fields are rejected. The file is limited to 64 KiB.
Environment variables do not override TOML settings.

### Device fields

| Field | Required | Rule |
|---|:---:|---|
| `device.id` | Yes | 1–64 ASCII letters, digits, `_`, or `-`; keep stable after deployment |
| `device.name` | Yes | 1–128 characters, trimmed, with no control characters |

`device.id` is part of the Discovery topic, unique IDs, and initial Home
Assistant entity IDs. Changing it creates a new Home Assistant device identity.
Choose a non-sensitive logical ID; do not use an IP address, MAC address,
credential, or personally identifying value.

### MQTT fields

| Field | Required | Default or rule |
|---|:---:|---|
| `mqtt.host` | No | `127.0.0.1`; hostname/address only, with no URI scheme, path, or credentials |
| `mqtt.port` | No | `1883`; range 1–65535 |
| `mqtt.client_id` | Yes | 1–128 ASCII letters, digits, `.`, `_`, or `-` |
| `mqtt.username` | Yes | 1–256 characters, trimmed, with no control characters |
| `mqtt.password_file` | Yes | Absolute path to a regular UTF-8 file |
| `mqtt.base_topic` | Yes | Concrete MQTT topic root |
| `mqtt.discovery_prefix` | No | `homeassistant`; concrete MQTT topic root |
| `mqtt.keep_alive_seconds` | No | `30`; range 5–3600 |

A concrete topic is non-empty, at most 65,535 bytes, has no leading/trailing
slash or empty level, contains no control characters, and contains neither `+`
nor `#`. Derived state, availability, and Discovery topics must also fit the
MQTT topic length limit.

The MQTT client uses a clean session, a 64 KiB packet limit, a small in-flight
window, and bounded request channels. Authentication failures are logged without
the password. Transport failures reconnect indefinitely with exponential
backoff capped at 60 seconds and bounded jitter.

### Collector fields

The entire `[collector]` section may be omitted to use defaults.

| Field | Required | Default or rule |
|---|:---:|---|
| `collector.interval_seconds` | No | `30`; range 5–86400 |
| `collector.root_filesystem` | No | `/`; absolute filesystem mount path |
| `collector.vcgencmd_path` | No | `/usr/bin/vcgencmd`; absolute path |
| `collector.command_timeout_seconds` | No | `2`; range 1–30 and shorter than the collection interval |

The configured `vcgencmd` path has priority. If it is not a regular file, the
collector checks `/usr/bin/vcgencmd` and then `/opt/vc/bin/vcgencmd`.
Startup fails clearly when none exists.

Collection uses:

| Measurement | Primary source | Fallback or behavior |
|---|---|---|
| CPU utilization | aggregate line in `/proc/stat` | First observation waits about one second for a real delta |
| Load average | `/proc/loadavg` | Nullable on failure |
| Temperature | `/sys/class/thermal/thermal_zone0/temp` | `vcgencmd measure_temp` only when the file is absent |
| CPU frequency | `scaling_cur_freq`, then `cpuinfo_cur_freq` in CPU0 cpufreq sysfs | Optional; absence is not fatal |
| Memory and swap | `/proc/meminfo` | Calculates an availability fallback if `MemAvailable` is absent |
| Filesystem | `statvfs` on `root_filesystem` | Available space uses the unprivileged block count |
| Uptime | `/proc/uptime` | Nullable on failure |
| Power flags | direct `vcgencmd get_throttled` process | One call per observation, with the configured deadline |

The command runner invokes the executable directly and does not use a shell.

## Credential file

Create the password file only after the service account exists:

```sh
sudo install -o root -g rpi-health-mqtt -m 0640 /dev/null \
  /etc/rpi-health-mqtt/mqtt-password
sudoedit /etc/rpi-health-mqtt/mqtt-password
sudo chown root:rpi-health-mqtt /etc/rpi-health-mqtt/mqtt-password
sudo chmod 0640 /etc/rpi-health-mqtt/mqtt-password
```

Enter only the MQTT password. Do not put the username, quotes, a TOML key, or a
broker URI in this file. The loader removes exactly one final LF or CRLF.
Other leading or trailing whitespace is part of the password.

On Unix, group write access and every permission for “other” users are rejected.
The file must be a regular file, valid UTF-8, non-empty after final-line-ending
removal, and no larger than 65,536 bytes. After removing the optional final LF
or CRLF, the credential itself may contain at most 65,535 UTF-8 bytes. The file
limit therefore permits a maximum-length credential followed by LF; all other
preserved whitespace counts toward the credential limit.

Do not pass the password on a command line: process listings and shell history
can expose it. The password value is redacted from debug output and is not
printed by `check`.

## Broker ACLs and bridge

The service publishes exactly three topic classes. Substitute the selected
configuration values:

```text
<base_topic>/state
<base_topic>/availability
<discovery_prefix>/device/<device.id>/config
```

A minimal conceptual local Mosquitto ACL is:

```conf
user <service-mqtt-user>
topic write <base_topic>/#
topic write <discovery_prefix>/device/<device.id>/config
```

No read permission or subscription is required by the daemon. Do not grant a
global wildcard unless the surrounding deployment explicitly requires it.

For an outbound bridge:

- forward `<base_topic>/#`;
- forward the exact Device Discovery configuration topic;
- preserve retained messages for Discovery and availability;
- allow QoS 0 state and QoS 1 Discovery/availability;
- authorize the bridge identity to write those topics on the destination; and
- authorize the Home Assistant MQTT identity to read them.

The service always connects to the configured broker; it has no awareness of
the bridge or destination broker. A successful test on a different topic proves
basic connectivity but does not verify that these topic patterns are forwarded.

## Preflight diagnostics

Run diagnostics with exactly the same account and configuration as systemd:

```sh
sudo -u rpi-health-mqtt /usr/local/bin/rpi-health-mqtt \
  --config /etc/rpi-health-mqtt/config.toml check
```

To exercise all local prerequisites without contacting the broker, add
`--skip-mqtt`:

```sh
sudo -u rpi-health-mqtt /usr/local/bin/rpi-health-mqtt \
  --config /etc/rpi-health-mqtt/config.toml check --skip-mqtt
```

The report covers configuration and credential loading, service identity and
groups, architecture, required local files, `vcgencmd` execution and parsing,
filesystem access, a collection probe, and a short authenticated broker
connection. The probe does not publish Discovery, availability, or state and
does not register a last will. `--skip-mqtt` omits only this connection; it does
not skip credential loading, collection, identity, or architecture checks.

Warnings identify non-production conditions that do not block the process, such
as running the command on a non-ARM development machine. A failed required check
returns exit code 4. This includes an unreadable or unsafe credential file and a
failed MQTT probe. Configuration syntax, fields, and file errors are rejected
before the report and return exit code 3. When starting the daemon rather than
running `check`, a credential-loading error also returns code 3. Warnings and an
explicitly skipped MQTT probe return code 0 when no required check fails.

For reference, process exit codes are:

| Code | Meaning |
|---:|---|
| 0 | Success, including diagnostic warnings or an explicitly skipped MQTT probe |
| 2 | Command-line usage error |
| 3 | Configuration-file error, or daemon credential-loading error |
| 4 | Required `check` prerequisite failed |
| 5 | `print-once` could not start collection or serialize its result |
| 6 | Unrecoverable daemon startup or runtime error |

Inspect one real payload without contacting MQTT:

```sh
sudo -u rpi-health-mqtt /usr/local/bin/rpi-health-mqtt \
  --config /etc/rpi-health-mqtt/config.toml print-once
```

Only formatted JSON is written to stdout. Logs and errors go to stderr. The
first CPU utilization calculation waits about one second so it can use a real
counter delta.

## Start and operate the service

After `check` and `print-once` succeed:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now rpi-health-mqtt.service
systemctl status rpi-health-mqtt.service
journalctl -u rpi-health-mqtt.service -f
```

The unit runs as `rpi-health-mqtt:rpi-health-mqtt`, with supplementary
membership in `video`, an empty capability set, `NoNewPrivileges`, protected
kernel/system paths, a private temporary directory, and a restrictive umask.
`PrivateDevices` is intentionally omitted because `vcgencmd` may require
`/dev/vcio` on Raspberry Pi OS.

Logs go only to stdout/stderr and therefore journald. The service creates no log
file and performs no periodic application disk writes.

The normal collection interval uses a monotonic timer. A tick that arrives while
collection is busy is skipped and produces at most one rate-limited warning.
Bounded, latest-only channels coalesce triggers rather than building a backlog.
After reconnecting, the daemon discards any disconnected observation, requests
one fresh collection, and does not replay missed states.

`SIGTERM` and `SIGINT` stop scheduling and attempt to publish retained `offline`,
receive its QoS 1 acknowledgement, and disconnect within a bounded deadline.
Transport loss makes that publication best effort; an abrupt termination uses
the broker's retained `offline` last will. The shutdown bounds fit the unit's
10-second service-manager deadline.

Measure runtime resources on the Raspberry Pi:

```sh
ps -o pid,pcpu,pmem,rss,vsz,etime,cmd -C rpi-health-mqtt
journalctl -u rpi-health-mqtt.service --since today
```

With a 30-second interval, initial acceptance targets are below 1% average CPU
and below 25 MiB RSS. The preferred goals are below 0.5% CPU and 15 MiB RSS.

## MQTT publications

| Publication | QoS | Retained | Payload |
|---|---:|:---:|---|
| Device Discovery | 1 | Yes | One JSON document containing all components |
| Availability after connection | 1 | Yes | `online` |
| Graceful shutdown or last will | 1 | Yes | `offline` |
| Periodic state | 0 | No | Compact schema-versioned JSON |

Discovery is published first and must be acknowledged. Online availability is
then published and acknowledged. The daemon requests a fresh collection for the
connection and publishes that non-retained state next. Old disconnected samples
are not replayed.

Every reconnect repeats this sequence. Reconnection delays use approximately
1, 2, 4, 8, 16, 30, and at most 60 seconds, with bounded jitter.

## State payload

The schema version is currently 1. An abbreviated complete-shape example is:

```json
{
  "schema_version": 1,
  "observed_at": "2026-01-01T00:00:00Z",
  "uptime_seconds": 123456,
  "cpu": {
    "usage_percent": 4.2,
    "load_1": 0.06,
    "load_5": 0.10,
    "load_15": 0.13,
    "temperature_c": 49.8,
    "frequency_mhz": 900.0
  },
  "memory": {
    "total_bytes": 964689920,
    "available_bytes": 702545920,
    "used_bytes": 262144000,
    "used_percent": 27.2
  },
  "swap": {
    "total_bytes": 963641344,
    "used_bytes": 0,
    "used_percent": 0.0
  },
  "disk": {
    "mount": "/",
    "total_bytes": 62277025792,
    "available_bytes": 56000000000,
    "used_bytes": 6200000000,
    "used_percent": 10.0
  },
  "power": {
    "throttled_raw": "0x0",
    "undervoltage_now": false,
    "arm_frequency_capped_now": false,
    "throttled_now": false,
    "soft_temperature_limit_now": false,
    "undervoltage_since_boot": false,
    "arm_frequency_capped_since_boot": false,
    "throttled_since_boot": false,
    "soft_temperature_limit_since_boot": false
  },
  "health": {
    "status": "ok",
    "collector_errors": []
  },
  "service": {
    "version": "0.1.0",
    "collection_duration_ms": 4
  }
}
```

Numeric or Boolean measurements that cannot be collected are `null`.
`health.collector_errors` contains stable metric names and sanitized,
non-secret descriptions. One metric failure does not discard unrelated data.

Health precedence is current critical firmware condition, incomplete collection,
historical firmware condition, then healthy:

```text
critical > degraded > warning > ok
```

The eight firmware fields map to bits 0–3 and 16–19 of
`vcgencmd get_throttled`. Since-boot bits remain set until reboot. They do not
record time or event count.

## Home Assistant entities

One Device Discovery message creates 26 entities. Every component is in the
`diagnostic` entity category and shares state, availability, and an expiry of
`max(interval_seconds * 3, 90)`.

| Component key | Platform | Name | Unit/class | Enabled by default |
|---|---|---|---|:---:|
| `health_status` | sensor | Health status | enum | Yes |
| `cpu_temperature` | sensor | CPU temperature | °C | Yes |
| `cpu_usage` | sensor | CPU usage | % | Yes |
| `load_1` | sensor | 1-minute load average | measurement | No |
| `load_5` | sensor | 5-minute load average | measurement | No |
| `load_15` | sensor | 15-minute load average | measurement | No |
| `cpu_frequency` | sensor | CPU frequency | MHz | No |
| `memory_usage` | sensor | Memory usage | % | Yes |
| `memory_available` | sensor | Memory available | GiB | No |
| `swap_usage` | sensor | Swap usage | % | Yes |
| `swap_used` | sensor | Swap used | MiB | No |
| `disk_usage` | sensor | Disk usage | % | Yes |
| `disk_available` | sensor | Disk available | GiB | Yes |
| `uptime` | sensor | Uptime | seconds | Yes |
| `last_observation` | sensor | Last observation | timestamp | No |
| `throttled_raw` | sensor | Raw throttling flags | hexadecimal text | No |
| `collection_duration` | sensor | Collection duration | milliseconds | No |
| `collector_error_count` | sensor | Collector error count | count | No |
| `undervoltage_now` | binary sensor | Current undervoltage | problem | Yes |
| `undervoltage_since_boot` | binary sensor | Undervoltage since boot | problem | Yes |
| `throttled_now` | binary sensor | Current throttling | problem | Yes |
| `throttled_since_boot` | binary sensor | Throttling since boot | problem | Yes |
| `arm_frequency_capped_now` | binary sensor | ARM frequency capped | problem | No |
| `arm_frequency_capped_since_boot` | binary sensor | ARM frequency capped since boot | problem | No |
| `soft_temperature_limit_now` | binary sensor | Soft temperature limit | problem | No |
| `soft_temperature_limit_since_boot` | binary sensor | Soft temperature limit since boot | problem | No |

The health enum options are `ok`, `warning`, `critical`, and `degraded`.
Unknown binary values remain unavailable rather than becoming `OFF`.

The stable unique ID is `<device.id>_<component-key>`. A normalized device ID
is used for the initial `default_entity_id`. Home Assistant owns any later
entity ID customization.

If entities do not appear, inspect the exact retained Discovery topic on both
the local and destination brokers, confirm the Home Assistant MQTT integration
uses the destination broker, and verify bridge/ACL coverage for the Discovery
prefix.

## Release artifacts

A tag named exactly `vVERSION`, where `VERSION` equals the package version in
`Cargo.toml`, starts the release workflow. Publication also requires the tagged
commit to be contained in the repository's main branch. The workflow validates
the repository, builds the ARMv7 binary twice in separate directories, requires
the two results to be byte-identical, verifies the ELF32 ARM hard-float ABI
under QEMU, and creates these public assets:

| Asset | Purpose |
|---|---|
| `rpi-health-mqtt-VERSION-armv7-unknown-linux-gnueabihf.tar.gz` | Binary and deployment files |
| `rpi-health-mqtt-VERSION-armv7-unknown-linux-gnueabihf.tar.gz.sha256` | Detached SHA-256 checksum for the archive |
| `rpi-health-mqtt-VERSION-armv7-unknown-linux-gnueabihf.spdx.json` | SPDX JSON software bill of materials (SBOM) |
| `SHA256SUMS` | Checksums for the archive and SBOM |

The archive also includes `LICENSE`, `THIRD-PARTY-LICENSES.html`, the public
configuration example, this guide, the installer and uninstaller, and the
systemd unit. A manual workflow dispatch performs the build and uploads a
short-lived workflow artifact for review; only a matching version tag on the
main branch publishes a GitHub release and provenance/SBOM attestations.

Download all release assets into one empty directory. From that directory,
verify both checksum forms before extraction:

```sh
sha256sum --check SHA256SUMS
sha256sum --check \
  rpi-health-mqtt-VERSION-armv7-unknown-linux-gnueabihf.tar.gz.sha256
```

Replace `VERSION` consistently with the release version. A checksum detects a
changed download only when the checksum file itself came from a trusted release
channel. For stronger provenance verification with GitHub CLI, use the actual
repository owner and name:

```sh
gh attestation verify \
  rpi-health-mqtt-VERSION-armv7-unknown-linux-gnueabihf.tar.gz \
  --repo OWNER/REPOSITORY
```

The SPDX JSON file inventories the packaged application and its dependencies;
it is not a signature. Its digest is covered by `SHA256SUMS`, while the release
attestation associates the SBOM with the archive.

When a repository checkout for the same version is available, perform the
additional archive structure, path, file-mode, manifest, and build-path audit:

```sh
bash scripts/verify-release-archive.sh \
  rpi-health-mqtt-VERSION-armv7-unknown-linux-gnueabihf.tar.gz \
  VERSION
```

This release verification does not replace checking target glibc compatibility
or running the deployment checklist on physical hardware.

## Upgrade

Build and validate the new version, then rerun the installer with its artifact:

```sh
sudo ./scripts/install.sh --binary /absolute/path/to/rpi-health-mqtt
systemctl status rpi-health-mqtt.service
journalctl -u rpi-health-mqtt.service -n 100 --no-pager
```

For a published archive, complete the checksum and attestation checks above,
extract it, enter its single top-level directory, and run `sudo
./scripts/install.sh`. The packaged binary is already at the installer's
default ARMv7 target path.

The operation replaces the binary and unit, preserves configuration and
password, reloads systemd, and restarts the enabled service when prerequisites
exist. Run `check` against the installed binary before or immediately after
the restart when configuration requirements change.

Keep `device.id`, `mqtt.client_id`, and topic roots stable to preserve Home
Assistant identity and avoid leaving retained configuration under an old topic.

## Uninstall

Remove the binary and unit while preserving configuration, password, and the
service account:

```sh
sudo ./scripts/uninstall.sh
```

Delete only the two managed configuration files as well:

```sh
sudo ./scripts/uninstall.sh --purge-config
```

With `--purge-config`, an otherwise-empty configuration directory is removed.
If it contains unrecognized files, the directory and those files are preserved.
The system account is always preserved so uninstalling cannot unexpectedly
change ownership semantics for local files.

The uninstaller does not delete retained MQTT messages. If the device is being
retired permanently, remove its retained Discovery and availability messages
through an authorized broker administration workflow.

## Troubleshooting

### Configuration is rejected

- Run `check` and read the named field; errors do not repeat secret TOML
  values.
- Remove unknown keys.
- Use absolute paths.
- Remove MQTT wildcards, leading/trailing slashes, and empty topic levels.
- Keep `command_timeout_seconds` shorter than `interval_seconds`.
- Keep `device.id` and `client_id` within their ASCII character sets.

### Credential file is rejected

```sh
sudo stat /etc/rpi-health-mqtt/mqtt-password
sudo chown root:rpi-health-mqtt /etc/rpi-health-mqtt/mqtt-password
sudo chmod 0640 /etc/rpi-health-mqtt/mqtt-password
```

Confirm it is a regular, non-empty UTF-8 file. Do not print its contents into a
support transcript. If authentication still fails, reset or verify the account
through the broker's own administration procedure.

### MQTT authentication or connection fails

- Confirm the broker listens on the configured loopback address and port.
- Confirm the service username exists on that broker.
- Confirm the password file contains only the matching password.
- Confirm local ACLs authorize all three publication topic classes.
- Inspect broker logs and service logs; neither should require revealing the
  password.
- Remember that temporary broker downtime causes retries rather than process
  termination.

### State is local but missing remotely

- Verify the bridge forwards `<base_topic>/#`.
- Verify both bridge-side ACLs.
- Check the exact same topic on the local and destination brokers.
- Confirm bridge direction is outbound from the device.
- Confirm QoS and retain propagation for availability.

### State arrives but Home Assistant creates no device

- Verify the exact retained
  `<discovery_prefix>/device/<device.id>/config` message on both brokers.
- Confirm bridge and ACL rules include the Discovery prefix, not just the state
  prefix.
- Confirm the Home Assistant MQTT integration's configured discovery prefix.
- Check Home Assistant logs for JSON/template errors.
- Do not repeatedly change `device.id`; it is the stable device identity.

### Entities become unavailable

All entities expire after three collection opportunities, with a minimum of 90
seconds. Check service status, journal logs, the local broker, the bridge, and
the destination broker in that order. A retained `online` value does not prove
that periodic state can currently cross a bridge.

### `vcgencmd` is missing or denied

```sh
command -v vcgencmd
id rpi-health-mqtt
sudo -u rpi-health-mqtt /usr/bin/vcgencmd get_throttled
```

Install the Raspberry Pi userland tools through the operating system package
manager when the command is absent. Confirm the account is in `video`, then
restart the service after changing group membership. On systems that require
`/dev/vcio`, verify its group and mode. Do not add `PrivateDevices=true` to
the unit unless firmware access has been independently proven.

### Temperature or CPU frequency is `null`

Temperature falls back to `vcgencmd measure_temp` only when the thermal sysfs
path does not exist. A present but malformed or unreadable sysfs file is
reported as an error instead of silently bypassed. CPU frequency is optional;
some kernels do not expose either supported cpufreq file.

Inspect `health.collector_errors` in `print-once` output. Other measurements
should remain useful after a partial failure.

### The executable will not start

```sh
file /usr/local/bin/rpi-health-mqtt
uname -m
dpkg --print-architecture
```

The executable must be ELF32 ARM hard-float for an `armhf` system. An
`aarch64-unknown-linux-gnu` build is incompatible with the target.

### systemd rejects a hardening directive

Run:

```sh
systemd-analyze verify /etc/systemd/system/rpi-health-mqtt.service
journalctl -u rpi-health-mqtt.service -n 100 --no-pager
```

The repository validates the unit against its development systemd version.
Raspberry Pi OS releases can differ. Remove only a directive proven unsupported
by the deployed version and document the local exception; preserve the service
identity, empty capabilities, restricted filesystem, and firmware access.

## Deployment validation checklist

Docker and QEMU cannot replace hardware and end-to-end deployment validation.
Complete these checks on the actual Raspberry Pi and receiving environment:

- [ ] `uname -m` and `dpkg --print-architecture` report ARMv7/armhf.
- [ ] `ldd --version` reports glibc 2.31 or newer for the published Docker
      artifact, or the binary was built with a sysroot compatible with the
      target.
- [ ] `file /usr/local/bin/rpi-health-mqtt` reports ELF32 ARM hard-float.
- [ ] Release checksums and provenance were verified before extracting a
      downloaded archive.
- [ ] `check` succeeds as the `rpi-health-mqtt` user without displaying a
      credential.
- [ ] `print-once` emits valid JSON with useful CPU, memory, swap, disk,
      uptime, temperature, and power data.
- [ ] The service user can execute `vcgencmd get_throttled` and access any
      required firmware device.
- [ ] The systemd unit verifies and starts under the installed Raspberry Pi OS
      systemd version.
- [ ] The local broker receives retained Discovery, retained `online`, and
      non-retained state on the configured topics.
- [ ] The same topics cross the bridge, including the Discovery prefix and
      retain flags.
- [ ] Home Assistant creates exactly one device and the expected 26 entities.
- [ ] A normal restart neither duplicates the device nor its entities.
- [ ] An abrupt process termination produces retained `offline` through the
      broker last will.
- [ ] Restarting the local broker triggers automatic reconnect, Discovery,
      `online`, and a fresh state without manual service restart.
- [ ] Blocking periodic state makes Home Assistant entities unavailable within
      the configured expiry window.
- [ ] Current and historical test bitmasks map to the expected power entities.
- [ ] Average CPU remains below 1% and RSS below 25 MiB at the normal interval.
- [ ] A continuous 24-hour run shows no unbounded memory growth or log spam.

Record the operating system and systemd versions with the deployment results.
Passing QEMU in the repository gate does not satisfy the glibc, firmware,
systemd, bridge, or resource-use checks above.
Never include the MQTT password, password-file contents, private hostnames, or
other site-specific secrets in a public report.
