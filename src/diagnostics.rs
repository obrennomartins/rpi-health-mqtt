//! Deterministic, secret-safe preflight diagnostics.
//!
//! The diagnostic runner exercises the same credential loader and collector as
//! the daemon. Its MQTT probe opens a short-lived clean session without a last
//! will or publications, then disconnects. Raw transport errors are mapped to
//! stable categories so credentials cannot enter operator-facing output.

use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use rumqttc::{
    AsyncClient, ConnectReturnCode, ConnectionError, Event, Incoming, MqttOptions, NetworkOptions,
    Outgoing,
};

use crate::{
    cli::{DIAGNOSTIC_ERROR, SUCCESS},
    collector::{Collector, DeviceMetadata},
    config::{Config, ConfigError, MqttConfig, MqttCredentials},
    model::{PowerMetrics, TelemetryState},
};

const MQTT_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const MQTT_CONNECTION_TIMEOUT_SECONDS: u64 = 3;
const MQTT_REQUEST_CAPACITY: usize = 4;
const MQTT_MAX_PACKET_BYTES: usize = 64 * 1024;
const MQTT_PROBE_CLIENT_ID_LIMIT: usize = 128;
const CREDENTIALS_CHECK: &str = "credentials";
const IDENTITY_CHECK: &str = "runtime identity";
const ARCHITECTURE_CHECK: &str = "architecture";
const COLLECTOR_CHECK: &str = "collector";
const METADATA_CHECK: &str = "metadata";
const POWER_CHECK: &str = "power";
const MQTT_CHECK: &str = "mqtt";

static MQTT_PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Outcome of one diagnostic check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckStatus {
    /// The prerequisite was exercised successfully.
    Pass,
    /// The prerequisite is usable, but an operator should review the result.
    Warning,
    /// The prerequisite prevents a safe or complete service start.
    Failure,
    /// The check was deliberately omitted or could not run after an earlier failure.
    Skipped,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warning => "WARNING",
            Self::Failure => "FAILURE",
            Self::Skipped => "SKIPPED",
        }
    }
}

/// One non-sensitive, operator-facing diagnostic result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticCheck {
    name: &'static str,
    status: CheckStatus,
    summary: String,
}

impl DiagnosticCheck {
    /// Returns the stable check name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the check outcome.
    #[must_use]
    pub fn status(&self) -> CheckStatus {
        self.status
    }

    /// Returns the secret-safe English summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    fn new(name: &'static str, status: CheckStatus, summary: impl Into<String>) -> Self {
        Self {
            name,
            status,
            summary: summary.into(),
        }
    }
}

/// Ordered results from a complete diagnostic run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticReport {
    checks: Vec<DiagnosticCheck>,
}

impl DiagnosticReport {
    /// Returns checks in their stable execution and rendering order.
    #[must_use]
    pub fn checks(&self) -> &[DiagnosticCheck] {
        &self.checks
    }

    /// Returns whether at least one blocking check failed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == CheckStatus::Failure)
    }

    /// Returns the stable process exit code for this report.
    ///
    /// Warnings and skipped optional checks do not make the diagnostic command
    /// fail. Any [`CheckStatus::Failure`] maps to the diagnostic-error code.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        if self.has_failures() {
            DIAGNOSTIC_ERROR
        } else {
            SUCCESS
        }
    }

    fn new(checks: Vec<DiagnosticCheck>) -> Self {
        Self { checks }
    }
}

impl fmt::Display for DiagnosticReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, check) in self.checks.iter().enumerate() {
            if index != 0 {
                formatter.write_str("\n")?;
            }
            write!(
                formatter,
                "[{}] {}: {}",
                check.status.label(),
                check.name,
                check.summary
            )?;
        }
        Ok(())
    }
}

/// Controls whether the preflight run contacts the configured MQTT broker.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MqttProbe {
    /// Open a bounded, authenticated connection and publish nothing.
    #[default]
    Enabled,
    /// Report the MQTT check as skipped without opening a socket.
    Skipped,
}

/// Runs all preflight checks and returns them in a stable order.
///
/// Configuration syntax is expected to have been validated by [`Config::load`]
/// before this function is called. Credential contents and raw MQTT errors are
/// never included in the returned report.
pub async fn run_checks(config: &Config, mqtt_probe: MqttProbe) -> DiagnosticReport {
    let (credential_check, credentials) = load_credentials(config);
    let mut checks = Vec::with_capacity(7);
    checks.push(credential_check);
    checks.push(check_runtime_identity());
    checks.push(check_architecture());

    match Collector::new(config.collector()) {
        Ok(mut collector) => {
            let metadata = collector.metadata().clone();
            let snapshot = collector.collect();
            checks.extend(check_snapshot(&metadata, &snapshot));
        }
        Err(_) => {
            checks.push(DiagnosticCheck::new(
                COLLECTOR_CHECK,
                CheckStatus::Failure,
                "vcgencmd was not found at the configured or standard locations",
            ));
            checks.push(DiagnosticCheck::new(
                METADATA_CHECK,
                CheckStatus::Skipped,
                "hardware metadata was not checked because the collector could not start",
            ));
            checks.push(DiagnosticCheck::new(
                POWER_CHECK,
                CheckStatus::Skipped,
                "power flags were not checked because the collector could not start",
            ));
        }
    }

    checks.push(match mqtt_probe {
        MqttProbe::Skipped => DiagnosticCheck::new(
            MQTT_CHECK,
            CheckStatus::Skipped,
            "broker connectivity was explicitly skipped",
        ),
        MqttProbe::Enabled => match credentials.as_ref() {
            Some(credentials) => check_mqtt(config.mqtt(), credentials).await,
            None => DiagnosticCheck::new(
                MQTT_CHECK,
                CheckStatus::Skipped,
                "broker connectivity requires a valid credential file",
            ),
        },
    });

    DiagnosticReport::new(checks)
}

fn load_credentials(config: &Config) -> (DiagnosticCheck, Option<MqttCredentials>) {
    match MqttCredentials::load(config.mqtt()) {
        Ok(credentials) => (
            DiagnosticCheck::new(
                CREDENTIALS_CHECK,
                CheckStatus::Pass,
                "credential file is readable and passes permission checks",
            ),
            Some(credentials),
        ),
        Err(error) => (
            DiagnosticCheck::new(
                CREDENTIALS_CHECK,
                CheckStatus::Failure,
                credential_failure_summary(&error),
            ),
            None,
        ),
    }
}

fn credential_failure_summary(error: &ConfigError) -> &'static str {
    match error {
        ConfigError::CredentialRead { .. } => "credential file could not be read",
        ConfigError::CredentialNotAFile(_) => "credential path is not a regular file",
        ConfigError::CredentialTooLarge(_) => "credential file exceeds the 64 KiB limit",
        ConfigError::CredentialEncoding(_) => "credential file is not valid UTF-8",
        ConfigError::CredentialEmpty(_) => "credential file is empty",
        ConfigError::CredentialTooLong(_) => "credential exceeds the MQTT UTF-8 field limit",
        ConfigError::CredentialPermissions(_) => {
            "credential file grants access beyond its owner and group"
        }
        ConfigError::Read { .. }
        | ConfigError::NotAFile(_)
        | ConfigError::TooLarge(_)
        | ConfigError::SourceTooLarge
        | ConfigError::Toml(_)
        | ConfigError::InvalidField { .. } => "credential validation could not be completed",
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProcessIdentity {
    effective_uid: u32,
    effective_gid: u32,
    supplementary_groups: Vec<u32>,
}

#[cfg(target_os = "linux")]
fn check_runtime_identity() -> DiagnosticCheck {
    identity_check(
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| parse_process_identity(&status)),
    )
}

#[cfg(not(target_os = "linux"))]
fn check_runtime_identity() -> DiagnosticCheck {
    DiagnosticCheck::new(
        IDENTITY_CHECK,
        CheckStatus::Failure,
        "runtime identity checks require Linux",
    )
}

fn identity_check(identity: Option<ProcessIdentity>) -> DiagnosticCheck {
    match identity {
        Some(identity) if identity.effective_uid == 0 => DiagnosticCheck::new(
            IDENTITY_CHECK,
            CheckStatus::Failure,
            "process has effective UID 0; run it as the dedicated service account",
        ),
        Some(identity)
            if identity.effective_gid == 0 || identity.supplementary_groups.contains(&0) =>
        {
            DiagnosticCheck::new(
                IDENTITY_CHECK,
                CheckStatus::Warning,
                "process is non-root but retains membership in group 0",
            )
        }
        Some(mut identity) => {
            identity.supplementary_groups.sort_unstable();
            identity.supplementary_groups.dedup();
            let groups = identity
                .supplementary_groups
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            DiagnosticCheck::new(
                IDENTITY_CHECK,
                CheckStatus::Pass,
                format!(
                    "effective UID {}, effective GID {}, supplementary groups [{groups}]",
                    identity.effective_uid, identity.effective_gid
                ),
            )
        }
        None => DiagnosticCheck::new(
            IDENTITY_CHECK,
            CheckStatus::Failure,
            "effective UID, GID, and supplementary groups could not be read",
        ),
    }
}

fn parse_process_identity(status: &str) -> Option<ProcessIdentity> {
    let uid = parse_status_numbers(status, "Uid:")?;
    let gid = parse_status_numbers(status, "Gid:")?;
    let supplementary_groups = parse_status_numbers(status, "Groups:")?;
    Some(ProcessIdentity {
        effective_uid: *uid.get(1)?,
        effective_gid: *gid.get(1)?,
        supplementary_groups,
    })
}

fn parse_status_numbers(status: &str, field: &str) -> Option<Vec<u32>> {
    let values = status
        .lines()
        .find_map(|line| line.strip_prefix(field))?
        .split_ascii_whitespace()
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if field == "Groups:" || !values.is_empty() {
        Some(values)
    } else {
        None
    }
}

fn check_architecture() -> DiagnosticCheck {
    architecture_check(std::env::consts::ARCH, cfg!(target_arch = "arm"))
}

fn architecture_check(architecture: &str, is_armv7: bool) -> DiagnosticCheck {
    if is_armv7 {
        DiagnosticCheck::new(
            ARCHITECTURE_CHECK,
            CheckStatus::Pass,
            format!("running on supported ARM architecture ({architecture})"),
        )
    } else {
        DiagnosticCheck::new(
            ARCHITECTURE_CHECK,
            CheckStatus::Warning,
            format!("running on {architecture}; deploy the ARMv7 build to Raspberry Pi 2"),
        )
    }
}

fn check_snapshot(metadata: &DeviceMetadata, snapshot: &TelemetryState) -> [DiagnosticCheck; 3] {
    let collector = if snapshot.health.collector_errors.is_empty() {
        DiagnosticCheck::new(
            COLLECTOR_CHECK,
            CheckStatus::Pass,
            "one complete snapshot was collected",
        )
    } else {
        let mut metrics = snapshot
            .health
            .collector_errors
            .iter()
            .map(|error| error.metric.as_str())
            .collect::<Vec<_>>();
        metrics.sort_unstable();
        metrics.dedup();
        let only_optional_frequency = metrics == ["cpu.frequency_mhz"];
        DiagnosticCheck::new(
            COLLECTOR_CHECK,
            if only_optional_frequency {
                CheckStatus::Warning
            } else {
                CheckStatus::Failure
            },
            format!(
                "snapshot reported {} unavailable metric source(s): {}",
                metrics.len(),
                metrics.join(", ")
            ),
        )
    };

    let missing_metadata = [
        ("model", metadata.model.is_none()),
        ("board revision", metadata.board_revision.is_none()),
        ("kernel release", metadata.kernel_release.is_none()),
        ("operating system", metadata.operating_system.is_none()),
    ]
    .into_iter()
    .filter_map(|(name, missing)| missing.then_some(name))
    .collect::<Vec<_>>();
    let metadata = if missing_metadata.is_empty() {
        DiagnosticCheck::new(
            METADATA_CHECK,
            CheckStatus::Pass,
            "model, board revision, kernel release, and operating system metadata are available",
        )
    } else {
        DiagnosticCheck::new(
            METADATA_CHECK,
            CheckStatus::Warning,
            format!("metadata unavailable: {}", missing_metadata.join(", ")),
        )
    };

    let power = check_power(&snapshot.power);
    [collector, metadata, power]
}

fn check_power(power: &PowerMetrics) -> DiagnosticCheck {
    if power.has_current_problem() {
        DiagnosticCheck::new(
            POWER_CHECK,
            CheckStatus::Warning,
            "a current undervoltage, throttling, frequency-cap, or thermal-limit flag is active",
        )
    } else if power.has_historical_problem() {
        DiagnosticCheck::new(
            POWER_CHECK,
            CheckStatus::Warning,
            "a historical undervoltage, throttling, frequency-cap, or thermal-limit flag is set",
        )
    } else if power.throttled_raw.is_some() {
        DiagnosticCheck::new(
            POWER_CHECK,
            CheckStatus::Pass,
            "firmware power flags are available and clear",
        )
    } else {
        DiagnosticCheck::new(
            POWER_CHECK,
            CheckStatus::Skipped,
            "power flags are unavailable; the collector result reports the cause",
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MqttProbeFailure {
    Rejected,
    Unreachable,
    TimedOut,
    Protocol,
    ControlQueue,
}

async fn check_mqtt(config: &MqttConfig, credentials: &MqttCredentials) -> DiagnosticCheck {
    match probe_mqtt(config, credentials).await {
        Ok(()) => DiagnosticCheck::new(
            MQTT_CHECK,
            CheckStatus::Pass,
            "authenticated clean-session connection succeeded without publishing",
        ),
        Err(failure) => mqtt_failure_check(failure),
    }
}

fn mqtt_failure_check(failure: MqttProbeFailure) -> DiagnosticCheck {
    let summary = match failure {
        MqttProbeFailure::Rejected => {
            "broker rejected the diagnostic connection; verify credentials and connection policy"
        }
        MqttProbeFailure::Unreachable => {
            "broker address could not be resolved or reached before the deadline"
        }
        MqttProbeFailure::TimedOut => "broker connection timed out after three seconds",
        MqttProbeFailure::Protocol => "broker returned an invalid or unsupported MQTT response",
        MqttProbeFailure::ControlQueue => "diagnostic MQTT control queue was unavailable",
    };
    DiagnosticCheck::new(MQTT_CHECK, CheckStatus::Failure, summary)
}

async fn probe_mqtt(
    config: &MqttConfig,
    credentials: &MqttCredentials,
) -> Result<(), MqttProbeFailure> {
    let options = probe_options(config, credentials);
    let (client, mut event_loop) = AsyncClient::new(options, MQTT_REQUEST_CAPACITY);
    let mut network_options = NetworkOptions::new();
    network_options.set_connection_timeout(MQTT_CONNECTION_TIMEOUT_SECONDS);
    event_loop.set_network_options(network_options);

    let probe = async {
        let mut connected = false;
        loop {
            match event_loop.poll().await {
                Ok(Event::Incoming(Incoming::ConnAck(acknowledgement)))
                    if acknowledgement.code == ConnectReturnCode::Success =>
                {
                    connected = true;
                    client
                        .try_disconnect()
                        .map_err(|_| MqttProbeFailure::ControlQueue)?;
                }
                Ok(Event::Outgoing(Outgoing::Disconnect)) if connected => return Ok(()),
                Ok(_) => {}
                Err(_) if connected => return Ok(()),
                Err(error) => return Err(classify_connection_error(&error)),
            }
        }
    };

    tokio::time::timeout(MQTT_PROBE_TIMEOUT, probe)
        .await
        .unwrap_or(Err(MqttProbeFailure::TimedOut))
}

fn probe_options(config: &MqttConfig, credentials: &MqttCredentials) -> MqttOptions {
    let mut options = MqttOptions::new(
        unique_probe_client_id(config.client_id()),
        config.host(),
        config.port(),
    );
    options
        .set_clean_session(true)
        .set_keep_alive(config.keep_alive().min(Duration::from_secs(30)))
        .set_credentials(credentials.username(), credentials.expose_password())
        .set_max_packet_size(MQTT_MAX_PACKET_BYTES, MQTT_MAX_PACKET_BYTES)
        .set_request_channel_capacity(MQTT_REQUEST_CAPACITY)
        .set_inflight(1);
    options
}

fn unique_probe_client_id(base: &str) -> String {
    let sequence = MQTT_PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let suffix = format!("-check-{}-{sequence}", std::process::id());
    let prefix_length = MQTT_PROBE_CLIENT_ID_LIMIT.saturating_sub(suffix.len());
    let prefix = &base[..base.len().min(prefix_length)];
    format!("{prefix}{suffix}")
}

fn classify_connection_error(error: &ConnectionError) -> MqttProbeFailure {
    match error {
        ConnectionError::ConnectionRefused(_) => MqttProbeFailure::Rejected,
        ConnectionError::NetworkTimeout | ConnectionError::FlushTimeout => {
            MqttProbeFailure::TimedOut
        }
        ConnectionError::Io(_) => MqttProbeFailure::Unreachable,
        ConnectionError::MqttState(_)
        | ConnectionError::NotConnAck(_)
        | ConnectionError::RequestsDone => MqttProbeFailure::Protocol,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use crate::model::{CollectorError, ObservationTime, TelemetryReadings};

    use super::*;

    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
    const CANARY_SECRET: &str = "diagnostic-canary-secret-do-not-print";

    #[test]
    fn report_rendering_and_order_are_stable() {
        let report = DiagnosticReport::new(vec![
            DiagnosticCheck::new(CREDENTIALS_CHECK, CheckStatus::Pass, "ready"),
            DiagnosticCheck::new(ARCHITECTURE_CHECK, CheckStatus::Warning, "review"),
            DiagnosticCheck::new(MQTT_CHECK, CheckStatus::Skipped, "omitted"),
        ]);

        assert_eq!(
            report.to_string(),
            "[PASS] credentials: ready\n[WARNING] architecture: review\n[SKIPPED] mqtt: omitted"
        );
        assert_eq!(
            report
                .checks()
                .iter()
                .map(DiagnosticCheck::name)
                .collect::<Vec<_>>(),
            [CREDENTIALS_CHECK, ARCHITECTURE_CHECK, MQTT_CHECK]
        );
    }

    #[test]
    fn only_failures_change_the_report_exit_code() {
        for status in [
            CheckStatus::Pass,
            CheckStatus::Warning,
            CheckStatus::Skipped,
        ] {
            let report = DiagnosticReport::new(vec![DiagnosticCheck::new("example", status, "ok")]);
            assert!(!report.has_failures());
            assert_eq!(report.exit_code(), SUCCESS);
        }

        let report = DiagnosticReport::new(vec![DiagnosticCheck::new(
            "example",
            CheckStatus::Failure,
            "blocked",
        )]);
        assert!(report.has_failures());
        assert_eq!(report.exit_code(), DIAGNOSTIC_ERROR);
    }

    #[test]
    fn process_identity_parses_effective_values_and_groups() {
        let identity = parse_process_identity(
            "Name:\texample\nUid:\t1000\t1001\t1002\t1003\nGid:\t2000\t2001\t2002\t2003\nGroups:\t44 100 2001\n",
        )
        .expect("identity should parse");

        assert_eq!(identity.effective_uid, 1001);
        assert_eq!(identity.effective_gid, 2001);
        assert_eq!(identity.supplementary_groups, [44, 100, 2001]);
    }

    #[test]
    fn process_identity_requires_complete_numeric_fields() {
        for status in [
            "Uid:\t1000\nGid:\t1000\t1000\nGroups:\t44\n",
            "Uid:\t1000\tinvalid\nGid:\t1000\t1000\nGroups:\t44\n",
            "Uid:\t1000\t1000\nGroups:\t44\n",
            "Uid:\t1000\t1000\nGid:\t1000\t1000\n",
        ] {
            assert_eq!(parse_process_identity(status), None);
        }
    }

    #[test]
    fn only_armv7_architecture_is_reported_as_supported() {
        assert_eq!(architecture_check("arm", true).status(), CheckStatus::Pass);
        for architecture in ["aarch64", "x86", "x86_64"] {
            let check = architecture_check(architecture, false);
            assert_eq!(check.status(), CheckStatus::Warning);
            assert!(check.summary().contains(architecture));
        }
    }

    #[test]
    fn identity_mapping_rejects_root_and_warns_about_group_zero() {
        let root = identity_check(Some(ProcessIdentity {
            effective_uid: 0,
            effective_gid: 0,
            supplementary_groups: vec![0],
        }));
        let privileged_group = identity_check(Some(ProcessIdentity {
            effective_uid: 995,
            effective_gid: 0,
            supplementary_groups: vec![44],
        }));
        let service = identity_check(Some(ProcessIdentity {
            effective_uid: 995,
            effective_gid: 995,
            supplementary_groups: vec![995, 44, 100, 44],
        }));

        assert_eq!(root.status(), CheckStatus::Failure);
        assert_eq!(privileged_group.status(), CheckStatus::Warning);
        assert_eq!(service.status(), CheckStatus::Pass);
        assert_eq!(
            service.summary(),
            "effective UID 995, effective GID 995, supplementary groups [44, 100, 995]"
        );
        assert_eq!(identity_check(None).status(), CheckStatus::Failure);
    }

    #[test]
    fn collector_error_metrics_are_sorted_and_blocking() {
        let state = state_with(
            vec![
                CollectorError::new("memory", "unavailable"),
                CollectorError::new("disk", "unavailable"),
                CollectorError::new("memory", "still unavailable"),
            ],
            clear_power(),
        );
        let checks = check_snapshot(&DeviceMetadata::default(), &state);

        assert_eq!(checks[0].status(), CheckStatus::Failure);
        assert_eq!(
            checks[0].summary(),
            "snapshot reported 2 unavailable metric source(s): disk, memory"
        );
        assert_eq!(checks[1].status(), CheckStatus::Warning);
        assert_eq!(checks[2].status(), CheckStatus::Pass);
    }

    #[test]
    fn optional_frequency_is_a_warning_but_mixed_failures_are_blocking() {
        let frequency_only = state_with(
            vec![CollectorError::new(
                "cpu.frequency_mhz",
                "frequency unavailable",
            )],
            clear_power(),
        );
        let mixed = state_with(
            vec![
                CollectorError::new("cpu.frequency_mhz", "frequency unavailable"),
                CollectorError::new("power", "power unavailable"),
            ],
            PowerMetrics::default(),
        );

        assert_eq!(
            check_snapshot(&complete_metadata(), &frequency_only)[0].status(),
            CheckStatus::Warning
        );
        assert_eq!(
            check_snapshot(&complete_metadata(), &mixed)[0].status(),
            CheckStatus::Failure
        );
    }

    #[test]
    fn metadata_and_power_mapping_are_deterministic() {
        let metadata = complete_metadata();
        let clean = check_snapshot(&metadata, &state_with(Vec::new(), clear_power()));
        assert_eq!(clean[0].status(), CheckStatus::Pass);
        assert_eq!(clean[1].status(), CheckStatus::Pass);
        assert_eq!(clean[2].status(), CheckStatus::Pass);

        let current = PowerMetrics {
            undervoltage_now: Some(true),
            ..clear_power()
        };
        let historical = PowerMetrics {
            throttled_since_boot: Some(true),
            ..clear_power()
        };
        assert_eq!(check_power(&current).status(), CheckStatus::Warning);
        assert_eq!(check_power(&historical).status(), CheckStatus::Warning);
        assert_eq!(
            check_power(&PowerMetrics::default()).status(),
            CheckStatus::Skipped
        );

        let incomplete = check_snapshot(
            &DeviceMetadata {
                kernel_release: Some("example".to_owned()),
                operating_system: Some("example".to_owned()),
                hostname: Some(CANARY_SECRET.to_owned()),
                serial_number: Some(CANARY_SECRET.to_owned()),
                ..DeviceMetadata::default()
            },
            &state_with(Vec::new(), clear_power()),
        );
        assert_eq!(incomplete[1].status(), CheckStatus::Warning);
        assert_eq!(
            incomplete[1].summary(),
            "metadata unavailable: model, board revision"
        );
        assert!(!incomplete[1].summary().contains(CANARY_SECRET));
    }

    #[test]
    fn probe_client_ids_are_unique_and_bounded() {
        let base = "x".repeat(MQTT_PROBE_CLIENT_ID_LIMIT);
        let first = unique_probe_client_id(&base);
        let second = unique_probe_client_id(&base);

        assert_ne!(first, second);
        assert!(first.len() <= MQTT_PROBE_CLIENT_ID_LIMIT);
        assert!(second.len() <= MQTT_PROBE_CLIENT_ID_LIMIT);
        assert!(first.contains("-check-"));
    }

    #[test]
    fn probe_options_use_a_clean_session_without_a_last_will() {
        let (config, directory) = fixture_config(CANARY_SECRET);
        let credentials = MqttCredentials::load(config.mqtt()).expect("credential should load");
        let options = probe_options(config.mqtt(), &credentials);

        assert!(options.clean_session());
        assert_eq!(options.last_will(), None);
        assert_eq!(options.inflight(), 1);
        assert_eq!(options.request_channel_capacity(), MQTT_REQUEST_CAPACITY);
        assert_eq!(options.max_packet_size(), MQTT_MAX_PACKET_BYTES);
        assert!(options.client_id().contains("-check-"));
        let login = options
            .credentials()
            .expect("credentials should be configured");
        assert!(login.password.as_bytes() == CANARY_SECRET.as_bytes());

        fs::remove_dir_all(directory).expect("fixture should be removable");
    }

    #[test]
    fn credential_and_mqtt_failures_never_render_canary_values() {
        let credential_error = ConfigError::CredentialRead {
            path: PathBuf::from(format!("/private/{CANARY_SECRET}")),
            source: io::Error::new(io::ErrorKind::PermissionDenied, CANARY_SECRET),
        };
        let oversized_credential_error =
            ConfigError::CredentialTooLong(PathBuf::from(format!("/private/{CANARY_SECRET}")));
        assert_eq!(
            credential_failure_summary(&oversized_credential_error),
            "credential exceeds the MQTT UTF-8 field limit"
        );
        let mut checks = vec![
            DiagnosticCheck::new(
                CREDENTIALS_CHECK,
                CheckStatus::Failure,
                credential_failure_summary(&credential_error),
            ),
            DiagnosticCheck::new(
                CREDENTIALS_CHECK,
                CheckStatus::Failure,
                credential_failure_summary(&oversized_credential_error),
            ),
        ];
        checks.extend([
            mqtt_failure_check(MqttProbeFailure::Rejected),
            mqtt_failure_check(MqttProbeFailure::Unreachable),
            mqtt_failure_check(MqttProbeFailure::TimedOut),
            mqtt_failure_check(MqttProbeFailure::Protocol),
            mqtt_failure_check(MqttProbeFailure::ControlQueue),
        ]);
        let report = DiagnosticReport::new(checks);
        let rendered = format!("{report}\n{report:?}");

        assert!(!rendered.contains(CANARY_SECRET));
        assert!(!rendered.contains("/private/"));
    }

    #[test]
    fn raw_connection_failures_map_to_sanitized_categories() {
        assert_eq!(
            classify_connection_error(&ConnectionError::NetworkTimeout),
            MqttProbeFailure::TimedOut
        );
        assert_eq!(
            classify_connection_error(&ConnectionError::Io(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                CANARY_SECRET,
            ))),
            MqttProbeFailure::Unreachable
        );
        assert_eq!(
            classify_connection_error(&ConnectionError::ConnectionRefused(
                ConnectReturnCode::NotAuthorized,
            )),
            MqttProbeFailure::Rejected
        );
    }

    fn state_with(errors: Vec<CollectorError>, power: PowerMetrics) -> TelemetryState {
        let mut readings = TelemetryReadings::unavailable("/");
        readings.power = power;
        TelemetryState::new(
            ObservationTime::parse("2026-08-16T12:00:00Z").expect("timestamp should parse"),
            readings,
            errors,
            1,
        )
    }

    fn clear_power() -> PowerMetrics {
        PowerMetrics {
            throttled_raw: Some("0x0".to_owned()),
            undervoltage_now: Some(false),
            arm_frequency_capped_now: Some(false),
            throttled_now: Some(false),
            soft_temperature_limit_now: Some(false),
            undervoltage_since_boot: Some(false),
            arm_frequency_capped_since_boot: Some(false),
            throttled_since_boot: Some(false),
            soft_temperature_limit_since_boot: Some(false),
        }
    }

    fn complete_metadata() -> DeviceMetadata {
        DeviceMetadata {
            model: Some("Example Model".to_owned()),
            board_revision: Some("example".to_owned()),
            kernel_release: Some("example".to_owned()),
            operating_system: Some("example".to_owned()),
            ..DeviceMetadata::default()
        }
    }

    fn fixture_config(secret: &str) -> (Config, PathBuf) {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "rpi-health-mqtt-diagnostics-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("fixture directory should be created");
        let password_path = directory.join("mqtt-password");
        fs::write(&password_path, format!("{secret}\n")).expect("credential should be written");
        restrict_fixture_permissions(&password_path);

        let password_toml = toml_path(&password_path);
        let root_toml = toml_path(root_path());
        let config = Config::parse(&format!(
            r#"
[device]
id = "example-pi"
name = "Example Raspberry Pi"

[mqtt]
host = "127.0.0.1"
port = 1883
client_id = "rpi-health-mqtt-example-pi"
username = "monitor-example"
password_file = "{password_toml}"
base_topic = "example/monitor/example-pi"

[collector]
root_filesystem = "{root_toml}"
vcgencmd_path = "{root_toml}vcgencmd"
"#
        ))
        .expect("fixture configuration should parse");
        (config, directory)
    }

    #[cfg(unix)]
    fn restrict_fixture_permissions(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("credential permissions should be restricted");
    }

    #[cfg(not(unix))]
    fn restrict_fixture_permissions(_path: &Path) {}

    #[cfg(unix)]
    fn root_path() -> &'static Path {
        Path::new("/")
    }

    #[cfg(windows)]
    fn root_path() -> &'static Path {
        Path::new("C:/")
    }

    fn toml_path(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "\\\\")
    }
}
