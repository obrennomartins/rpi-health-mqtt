//! Versioned telemetry payload types.
//!
//! The types in this module define the stable JSON state contract published by
//! the service. A missing measurement is represented by [`None`] and therefore
//! serialized as JSON `null`; known fields are never omitted. Floating-point
//! measurements are rounded at the serialization boundary and non-finite values
//! are rejected.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::{format_description::well_known::Rfc3339, OffsetDateTime, UtcOffset};

/// The current state payload schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// A UTC observation timestamp serialized in RFC 3339 format.
///
/// Parsed values with a non-UTC offset are converted to the equivalent UTC
/// instant so serialized payloads consistently end in `Z`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ObservationTime(OffsetDateTime);

impl ObservationTime {
    /// Returns the current wall-clock time in UTC.
    #[must_use]
    pub fn now_utc() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// Parses an RFC 3339 timestamp and normalizes it to UTC.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not a valid RFC 3339 timestamp.
    pub fn parse(value: &str) -> Result<Self, time::error::Parse> {
        OffsetDateTime::parse(value, &Rfc3339)
            .map(|timestamp| Self(timestamp.to_offset(UtcOffset::UTC)))
    }

    /// Returns the normalized UTC date and time.
    #[must_use]
    pub fn as_datetime(self) -> OffsetDateTime {
        self.0
    }
}

impl fmt::Display for ObservationTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.0.format(&Rfc3339).map_err(|_| fmt::Error)?;
        formatter.write_str(&value)
    }
}

impl FromStr for ObservationTime {
    type Err = time::error::Parse;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for ObservationTime {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = self.0.format(&Rfc3339).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&value)
    }
}

impl<'de> Deserialize<'de> for ObservationTime {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// CPU measurements collected for one observation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuMetrics {
    /// Aggregate CPU utilization as a percentage.
    #[serde(
        serialize_with = "serialize_optional_one_decimal",
        deserialize_with = "deserialize_optional_finite"
    )]
    pub usage_percent: Option<f64>,
    /// One-minute load average.
    #[serde(
        serialize_with = "serialize_optional_two_decimals",
        deserialize_with = "deserialize_optional_finite"
    )]
    pub load_1: Option<f64>,
    /// Five-minute load average.
    #[serde(
        serialize_with = "serialize_optional_two_decimals",
        deserialize_with = "deserialize_optional_finite"
    )]
    pub load_5: Option<f64>,
    /// Fifteen-minute load average.
    #[serde(
        serialize_with = "serialize_optional_two_decimals",
        deserialize_with = "deserialize_optional_finite"
    )]
    pub load_15: Option<f64>,
    /// CPU temperature in degrees Celsius.
    #[serde(
        serialize_with = "serialize_optional_one_decimal",
        deserialize_with = "deserialize_optional_finite"
    )]
    pub temperature_c: Option<f64>,
    /// Current CPU frequency in megahertz.
    #[serde(
        serialize_with = "serialize_optional_one_decimal",
        deserialize_with = "deserialize_optional_finite"
    )]
    pub frequency_mhz: Option<f64>,
}

/// Memory measurements collected for one observation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryMetrics {
    /// Total usable memory in bytes.
    pub total_bytes: Option<u64>,
    /// Memory currently available without swapping, in bytes.
    pub available_bytes: Option<u64>,
    /// Memory currently used, in bytes.
    pub used_bytes: Option<u64>,
    /// Memory currently used as a percentage of total usable memory.
    #[serde(
        serialize_with = "serialize_optional_one_decimal",
        deserialize_with = "deserialize_optional_finite"
    )]
    pub used_percent: Option<f64>,
}

/// Swap measurements collected for one observation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SwapMetrics {
    /// Total configured swap in bytes.
    pub total_bytes: Option<u64>,
    /// Swap currently used, in bytes.
    pub used_bytes: Option<u64>,
    /// Swap currently used as a percentage of total configured swap.
    #[serde(
        serialize_with = "serialize_optional_one_decimal",
        deserialize_with = "deserialize_optional_finite"
    )]
    pub used_percent: Option<f64>,
}

/// Filesystem measurements collected for one observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiskMetrics {
    /// Filesystem mount point measured by this object.
    pub mount: String,
    /// Total filesystem size in bytes.
    pub total_bytes: Option<u64>,
    /// Space available to an unprivileged user, in bytes.
    pub available_bytes: Option<u64>,
    /// Filesystem space currently used, in bytes.
    pub used_bytes: Option<u64>,
    /// Filesystem space currently used as a percentage of used plus available space.
    #[serde(
        serialize_with = "serialize_optional_one_decimal",
        deserialize_with = "deserialize_optional_finite"
    )]
    pub used_percent: Option<f64>,
}

impl DiskMetrics {
    /// Creates a filesystem measurement with all numeric values unavailable.
    #[must_use]
    pub fn unavailable(mount: impl Into<String>) -> Self {
        Self {
            mount: mount.into(),
            total_bytes: None,
            available_bytes: None,
            used_bytes: None,
            used_percent: None,
        }
    }
}

/// Power, throttling, and thermal-limit measurements for one observation.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PowerMetrics {
    /// Raw hexadecimal bitmask reported by `vcgencmd get_throttled`.
    pub throttled_raw: Option<String>,
    /// Whether undervoltage is currently detected.
    pub undervoltage_now: Option<bool>,
    /// Whether the ARM frequency is currently capped.
    pub arm_frequency_capped_now: Option<bool>,
    /// Whether throttling is currently active.
    pub throttled_now: Option<bool>,
    /// Whether the soft temperature limit is currently active.
    pub soft_temperature_limit_now: Option<bool>,
    /// Whether undervoltage has occurred since boot.
    pub undervoltage_since_boot: Option<bool>,
    /// Whether the ARM frequency has been capped since boot.
    pub arm_frequency_capped_since_boot: Option<bool>,
    /// Whether throttling has occurred since boot.
    pub throttled_since_boot: Option<bool>,
    /// Whether the soft temperature limit has occurred since boot.
    pub soft_temperature_limit_since_boot: Option<bool>,
}

impl PowerMetrics {
    /// Returns whether any known current power or throttling condition is active.
    #[must_use]
    pub fn has_current_problem(&self) -> bool {
        [
            self.undervoltage_now,
            self.arm_frequency_capped_now,
            self.throttled_now,
            self.soft_temperature_limit_now,
        ]
        .into_iter()
        .any(|flag| flag == Some(true))
    }

    /// Returns whether any known historical power or throttling condition is active.
    #[must_use]
    pub fn has_historical_problem(&self) -> bool {
        [
            self.undervoltage_since_boot,
            self.arm_frequency_capped_since_boot,
            self.throttled_since_boot,
            self.soft_temperature_limit_since_boot,
        ]
        .into_iter()
        .any(|flag| flag == Some(true))
    }
}

/// A non-sensitive collection failure included in the state payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CollectorError {
    /// Stable name of the metric or collector that failed.
    pub metric: String,
    /// Concise diagnostic message that does not contain secrets.
    pub message: String,
}

impl CollectorError {
    /// Creates a collection failure for a metric.
    #[must_use]
    pub fn new(metric: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            metric: metric.into(),
            message: message.into(),
        }
    }
}

/// Aggregate health classification for an observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// No current, historical, or collection problem is known.
    Ok,
    /// No current problem is known, but a historical flag is active.
    Warning,
    /// A current power, throttling, or thermal-limit problem is active.
    Critical,
    /// Collection failed and the complete current state cannot be guaranteed.
    Degraded,
}

impl HealthStatus {
    /// Evaluates aggregate health using the contract's precedence rules.
    ///
    /// A known current problem takes precedence over collection failures. A
    /// collection failure takes precedence over historical flags.
    #[must_use]
    pub fn evaluate(power: &PowerMetrics, has_collector_errors: bool) -> Self {
        if power.has_current_problem() {
            Self::Critical
        } else if has_collector_errors {
            Self::Degraded
        } else if power.has_historical_problem() {
            Self::Warning
        } else {
            Self::Ok
        }
    }
}

/// Aggregate health and collection diagnostics for one observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Health {
    /// Aggregate status derived from power flags and collection failures.
    pub status: HealthStatus,
    /// Non-sensitive errors encountered while collecting this observation.
    pub collector_errors: Vec<CollectorError>,
}

impl Health {
    /// Builds aggregate health from power metrics and collection errors.
    #[must_use]
    pub fn evaluate(power: &PowerMetrics, collector_errors: Vec<CollectorError>) -> Self {
        let status = HealthStatus::evaluate(power, !collector_errors.is_empty());
        Self {
            status,
            collector_errors,
        }
    }

    /// Recomputes the aggregate status after power metrics or errors change.
    pub fn refresh_status(&mut self, power: &PowerMetrics) {
        self.status = HealthStatus::evaluate(power, !self.collector_errors.is_empty());
    }
}

/// Service metadata included with every observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceMetrics {
    /// Running service version.
    pub version: String,
    /// End-to-end collection duration in milliseconds.
    pub collection_duration_ms: u64,
}

impl ServiceMetrics {
    /// Creates service metadata using the package version compiled into the binary.
    #[must_use]
    pub fn new(collection_duration_ms: u64) -> Self {
        Self {
            version: crate::VERSION.to_owned(),
            collection_duration_ms,
        }
    }
}

/// Measurements produced by one collection cycle before payload metadata is added.
#[derive(Clone, Debug, PartialEq)]
pub struct TelemetryReadings {
    /// System uptime in whole seconds.
    pub uptime_seconds: Option<u64>,
    /// CPU measurements.
    pub cpu: CpuMetrics,
    /// Memory measurements.
    pub memory: MemoryMetrics,
    /// Swap measurements.
    pub swap: SwapMetrics,
    /// Filesystem measurements.
    pub disk: DiskMetrics,
    /// Power, throttling, and thermal-limit measurements.
    pub power: PowerMetrics,
}

impl TelemetryReadings {
    /// Creates a reading set with all measurements unavailable.
    #[must_use]
    pub fn unavailable(disk_mount: impl Into<String>) -> Self {
        Self {
            uptime_seconds: None,
            cpu: CpuMetrics::default(),
            memory: MemoryMetrics::default(),
            swap: SwapMetrics::default(),
            disk: DiskMetrics::unavailable(disk_mount),
            power: PowerMetrics::default(),
        }
    }
}

/// Complete versioned state payload published for one observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TelemetryState {
    /// Version of the JSON state schema.
    pub schema_version: u32,
    /// UTC time at which the observation was made.
    pub observed_at: ObservationTime,
    /// System uptime in whole seconds, or `null` when unavailable.
    pub uptime_seconds: Option<u64>,
    /// CPU measurements.
    pub cpu: CpuMetrics,
    /// Memory measurements.
    pub memory: MemoryMetrics,
    /// Swap measurements.
    pub swap: SwapMetrics,
    /// Filesystem measurements.
    pub disk: DiskMetrics,
    /// Power, throttling, and thermal-limit measurements.
    pub power: PowerMetrics,
    /// Aggregate health and collection diagnostics.
    pub health: Health,
    /// Service metadata.
    pub service: ServiceMetrics,
}

impl TelemetryState {
    /// Creates a state payload using the current schema and service versions.
    #[must_use]
    pub fn new(
        observed_at: ObservationTime,
        readings: TelemetryReadings,
        collector_errors: Vec<CollectorError>,
        collection_duration_ms: u64,
    ) -> Self {
        let health = Health::evaluate(&readings.power, collector_errors);
        Self {
            schema_version: SCHEMA_VERSION,
            observed_at,
            uptime_seconds: readings.uptime_seconds,
            cpu: readings.cpu,
            memory: readings.memory,
            swap: readings.swap,
            disk: readings.disk,
            power: readings.power,
            health,
            service: ServiceMetrics::new(collection_duration_ms),
        }
    }

    /// Recomputes aggregate health after public payload fields are changed.
    pub fn refresh_health(&mut self) {
        self.health.refresh_status(&self.power);
    }
}

/// Rounds a finite measurement to a fixed number of decimal places.
///
/// Returns [`None`] for non-finite inputs, unsupported precision, or a result
/// that overflows the finite range of `f64`. Negative zero is normalized to
/// positive zero.
#[must_use]
pub fn rounded_metric(value: f64, decimal_places: u32) -> Option<f64> {
    if !value.is_finite() {
        return None;
    }

    let exponent = i32::try_from(decimal_places).ok()?;
    let factor = 10_f64.powi(exponent);
    if !factor.is_finite() {
        return None;
    }

    let rounded = (value * factor).round() / factor;
    if !rounded.is_finite() {
        return None;
    }

    Some(if rounded == 0.0 { 0.0 } else { rounded })
}

fn serialize_optional_one_decimal<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serialize_optional_f64::<S, 1>(value, serializer)
}

fn serialize_optional_two_decimals<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serialize_optional_f64::<S, 2>(value, serializer)
}

fn serialize_optional_f64<S, const DECIMAL_PLACES: u32>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        None => serializer.serialize_none(),
        Some(value) => rounded_metric(*value, DECIMAL_PLACES)
            .ok_or_else(|| serde::ser::Error::custom("measurement must be finite"))
            .and_then(|rounded| serializer.serialize_some(&rounded)),
    }
}

fn deserialize_optional_finite<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<f64>::deserialize(deserializer)?;
    match value {
        Some(value) if !value.is_finite() => {
            Err(serde::de::Error::custom("measurement must be finite"))
        }
        value => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    fn observation_time() -> ObservationTime {
        ObservationTime::parse("2026-08-16T12:34:56Z").expect("fixture timestamp is valid")
    }

    fn complete_readings() -> TelemetryReadings {
        TelemetryReadings {
            uptime_seconds: Some(123_456),
            cpu: CpuMetrics {
                usage_percent: Some(4.24),
                load_1: Some(0.064),
                load_5: Some(0.101),
                load_15: Some(0.129),
                temperature_c: Some(49.84),
                frequency_mhz: Some(900.04),
            },
            memory: MemoryMetrics {
                total_bytes: Some(964_689_920),
                available_bytes: Some(702_545_920),
                used_bytes: Some(262_144_000),
                used_percent: Some(27.174),
            },
            swap: SwapMetrics {
                total_bytes: Some(963_641_344),
                used_bytes: Some(0),
                used_percent: Some(-0.0),
            },
            disk: DiskMetrics {
                mount: "/".to_owned(),
                total_bytes: Some(62_277_025_792),
                available_bytes: Some(56_000_000_000),
                used_bytes: Some(6_200_000_000),
                used_percent: Some(10.04),
            },
            power: PowerMetrics {
                throttled_raw: Some("0x0".to_owned()),
                undervoltage_now: Some(false),
                arm_frequency_capped_now: Some(false),
                throttled_now: Some(false),
                soft_temperature_limit_now: Some(false),
                undervoltage_since_boot: Some(false),
                arm_frequency_capped_since_boot: Some(false),
                throttled_since_boot: Some(false),
                soft_temperature_limit_since_boot: Some(false),
            },
        }
    }

    #[test]
    fn complete_state_serializes_to_the_version_one_contract() {
        let state = TelemetryState::new(observation_time(), complete_readings(), Vec::new(), 4);
        let serialized = serde_json::to_value(state).expect("state should serialize");

        assert_eq!(
            serialized,
            json!({
                "schema_version": 1,
                "observed_at": "2026-08-16T12:34:56Z",
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
                    "total_bytes": 62277025792_u64,
                    "available_bytes": 56000000000_u64,
                    "used_bytes": 6200000000_u64,
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
                "health": { "status": "ok", "collector_errors": [] },
                "service": {
                    "version": crate::VERSION,
                    "collection_duration_ms": 4
                }
            })
        );
    }

    #[test]
    fn unavailable_measurements_are_explicit_null_values() {
        let state = TelemetryState::new(
            observation_time(),
            TelemetryReadings::unavailable("/data"),
            vec![CollectorError::new("cpu", "aggregate CPU line is missing")],
            7,
        );
        let serialized = serde_json::to_value(state).expect("state should serialize");

        for pointer in [
            "/uptime_seconds",
            "/cpu/usage_percent",
            "/cpu/load_1",
            "/cpu/load_5",
            "/cpu/load_15",
            "/cpu/temperature_c",
            "/cpu/frequency_mhz",
            "/memory/total_bytes",
            "/memory/available_bytes",
            "/memory/used_bytes",
            "/memory/used_percent",
            "/swap/total_bytes",
            "/swap/used_bytes",
            "/swap/used_percent",
            "/disk/total_bytes",
            "/disk/available_bytes",
            "/disk/used_bytes",
            "/disk/used_percent",
            "/power/throttled_raw",
            "/power/undervoltage_now",
            "/power/arm_frequency_capped_now",
            "/power/throttled_now",
            "/power/soft_temperature_limit_now",
            "/power/undervoltage_since_boot",
            "/power/arm_frequency_capped_since_boot",
            "/power/throttled_since_boot",
            "/power/soft_temperature_limit_since_boot",
        ] {
            assert_eq!(serialized.pointer(pointer), Some(&Value::Null), "{pointer}");
        }
        assert_eq!(serialized["disk"]["mount"], "/data");
        assert_eq!(serialized["health"]["status"], "degraded");
    }

    #[test]
    fn timestamps_are_normalized_to_utc_and_round_trip() {
        let timestamp = ObservationTime::parse("2026-08-16T09:34:56-03:00")
            .expect("offset timestamp should parse");
        assert_eq!(timestamp.to_string(), "2026-08-16T12:34:56Z");

        let serialized = serde_json::to_string(&timestamp).expect("timestamp should serialize");
        let deserialized: ObservationTime =
            serde_json::from_str(&serialized).expect("timestamp should deserialize");
        assert_eq!(deserialized, timestamp);
        assert!(serde_json::from_str::<ObservationTime>("\"not-a-timestamp\"").is_err());
    }

    #[test]
    fn health_status_obeys_contract_precedence() {
        let none = PowerMetrics::default();
        let historical = PowerMetrics {
            undervoltage_since_boot: Some(true),
            ..PowerMetrics::default()
        };
        let current = PowerMetrics {
            throttled_now: Some(true),
            undervoltage_since_boot: Some(true),
            ..PowerMetrics::default()
        };

        assert_eq!(HealthStatus::evaluate(&none, false), HealthStatus::Ok);
        assert_eq!(
            HealthStatus::evaluate(&historical, false),
            HealthStatus::Warning
        );
        assert_eq!(
            HealthStatus::evaluate(&historical, true),
            HealthStatus::Degraded
        );
        assert_eq!(
            HealthStatus::evaluate(&current, true),
            HealthStatus::Critical
        );
    }

    #[test]
    fn every_power_flag_has_the_expected_health_effect() {
        let current_conditions = [
            PowerMetrics {
                undervoltage_now: Some(true),
                ..PowerMetrics::default()
            },
            PowerMetrics {
                arm_frequency_capped_now: Some(true),
                ..PowerMetrics::default()
            },
            PowerMetrics {
                throttled_now: Some(true),
                ..PowerMetrics::default()
            },
            PowerMetrics {
                soft_temperature_limit_now: Some(true),
                ..PowerMetrics::default()
            },
        ];
        let historical_conditions = [
            PowerMetrics {
                undervoltage_since_boot: Some(true),
                ..PowerMetrics::default()
            },
            PowerMetrics {
                arm_frequency_capped_since_boot: Some(true),
                ..PowerMetrics::default()
            },
            PowerMetrics {
                throttled_since_boot: Some(true),
                ..PowerMetrics::default()
            },
            PowerMetrics {
                soft_temperature_limit_since_boot: Some(true),
                ..PowerMetrics::default()
            },
        ];

        for power in current_conditions {
            assert_eq!(
                HealthStatus::evaluate(&power, false),
                HealthStatus::Critical
            );
        }
        for power in historical_conditions {
            assert_eq!(HealthStatus::evaluate(&power, false), HealthStatus::Warning);
        }
    }

    #[test]
    fn explicit_false_and_unknown_flags_are_not_problems() {
        let power = PowerMetrics {
            undervoltage_now: Some(false),
            undervoltage_since_boot: Some(false),
            ..PowerMetrics::default()
        };

        assert!(!power.has_current_problem());
        assert!(!power.has_historical_problem());
        assert_eq!(HealthStatus::evaluate(&power, false), HealthStatus::Ok);
    }

    #[test]
    fn refresh_health_uses_changed_power_and_existing_errors() {
        let mut state = TelemetryState::new(
            observation_time(),
            TelemetryReadings::unavailable("/"),
            vec![CollectorError::new("temperature", "sensor unavailable")],
            1,
        );
        assert_eq!(state.health.status, HealthStatus::Degraded);

        state.power.undervoltage_now = Some(true);
        state.refresh_health();
        assert_eq!(state.health.status, HealthStatus::Critical);
    }

    #[test]
    fn non_finite_measurements_cannot_be_serialized() {
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut readings = TelemetryReadings::unavailable("/");
            readings.cpu.usage_percent = Some(invalid);
            let state = TelemetryState::new(observation_time(), readings, Vec::new(), 1);

            let error = serde_json::to_string(&state).expect_err("non-finite value must fail");
            assert!(error.to_string().contains("measurement must be finite"));
        }
    }

    #[test]
    fn metric_rounding_is_bounded_and_normalizes_negative_zero() {
        assert_eq!(rounded_metric(12.345, 2), Some(12.35));
        assert_eq!(rounded_metric(-0.04, 1), Some(0.0));
        assert_eq!(rounded_metric(f64::NAN, 1), None);
        assert_eq!(rounded_metric(f64::INFINITY, 1), None);
        assert_eq!(rounded_metric(f64::MAX, 1), None);
        assert_eq!(rounded_metric(1.0, u32::MAX), None);
    }

    #[test]
    fn state_round_trips_without_changing_the_contract() {
        let expected = TelemetryState::new(
            observation_time(),
            complete_readings(),
            vec![CollectorError::new(
                "frequency",
                "frequency file unavailable",
            )],
            12,
        );
        let serialized = serde_json::to_string(&expected).expect("state should serialize");
        let actual: TelemetryState =
            serde_json::from_str(&serialized).expect("state should deserialize");
        let reserialized = serde_json::to_value(actual).expect("state should serialize again");

        assert_eq!(
            reserialized,
            serde_json::from_str::<Value>(&serialized).expect("serialized state is valid JSON")
        );
    }
}
