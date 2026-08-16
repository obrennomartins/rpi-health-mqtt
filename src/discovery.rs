//! Home Assistant MQTT Device Discovery payload generation.
//!
//! This module describes every telemetry entity in one central table and emits
//! a single, deterministic Device Discovery document. The state and
//! availability topics are shared by all components.
//!
//! # Example
//!
//! ```
//! use std::time::Duration;
//!
//! use rpi_health_mqtt::discovery::{build_discovery_message, DiscoverySettings};
//!
//! let settings = DiscoverySettings::new(
//!     "node-01",
//!     "Utility room node",
//!     "site/monitor/node-01",
//!     "homeassistant",
//!     Duration::from_secs(30),
//! );
//! let message = build_discovery_message(&settings).expect("discovery should serialize");
//!
//! assert_eq!(message.topic, "homeassistant/device/node-01/config");
//! assert!(message.payload.contains("\"components\""));
//! ```

use std::{collections::BTreeMap, time::Duration};

use serde::Serialize;

const ORIGIN_NAME: &str = env!("CARGO_PKG_NAME");
const SOFTWARE_VERSION: &str = env!("CARGO_PKG_VERSION");
const MANUFACTURER: &str = "Raspberry Pi";
const ONLINE_PAYLOAD: &str = "online";
const OFFLINE_PAYLOAD: &str = "offline";
const MINIMUM_EXPIRE_AFTER_SECONDS: u64 = 90;
const HEALTH_OPTIONS: &[&str] = &["ok", "warning", "critical", "degraded"];

/// Settings used to create a Home Assistant Device Discovery message.
///
/// Topic fragments and the device identifier are expected to have already
/// passed application configuration validation. The builder only normalizes a
/// trailing topic separator so it cannot accidentally emit duplicate slashes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoverySettings {
    /// Stable MQTT and Home Assistant device identifier.
    pub device_id: String,
    /// Human-readable device name shown by Home Assistant.
    pub name: String,
    /// MQTT topic below which state and availability are published.
    pub base_topic: String,
    /// Home Assistant MQTT discovery prefix.
    pub discovery_prefix: String,
    /// Interval between periodic telemetry observations.
    pub interval: Duration,
    /// Optional hardware model reported in the Home Assistant device registry.
    pub model: Option<String>,
    /// Optional hardware serial number reported in the device registry.
    pub serial_number: Option<String>,
    /// Optional hardware revision reported in the device registry.
    pub hardware_version: Option<String>,
}

impl DiscoverySettings {
    /// Creates discovery settings without optional hardware metadata.
    #[must_use]
    pub fn new(
        device_id: impl Into<String>,
        name: impl Into<String>,
        base_topic: impl Into<String>,
        discovery_prefix: impl Into<String>,
        interval: Duration,
    ) -> Self {
        Self {
            device_id: device_id.into(),
            name: name.into(),
            base_topic: base_topic.into(),
            discovery_prefix: discovery_prefix.into(),
            interval,
            model: None,
            serial_number: None,
            hardware_version: None,
        }
    }

    /// Adds a hardware model to the advertised device metadata.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Adds a hardware serial number to the advertised device metadata.
    #[must_use]
    pub fn with_serial_number(mut self, serial_number: impl Into<String>) -> Self {
        self.serial_number = Some(serial_number.into());
        self
    }

    /// Adds a hardware revision to the advertised device metadata.
    #[must_use]
    pub fn with_hardware_version(mut self, hardware_version: impl Into<String>) -> Self {
        self.hardware_version = Some(hardware_version.into());
        self
    }

    /// Returns the Home Assistant Device Discovery configuration topic.
    #[must_use]
    pub fn discovery_topic(&self) -> String {
        format!(
            "{}/device/{}/config",
            self.discovery_prefix.trim_end_matches('/'),
            self.device_id
        )
    }

    /// Returns the shared state topic used by every discovered component.
    #[must_use]
    pub fn state_topic(&self) -> String {
        child_topic(&self.base_topic, "state")
    }

    /// Returns the shared availability topic used by every component.
    #[must_use]
    pub fn availability_topic(&self) -> String {
        child_topic(&self.base_topic, "availability")
    }

    /// Returns the entity expiry in seconds.
    ///
    /// Home Assistant receives three normal collection opportunities before an
    /// entity expires, with a minimum window of 90 seconds. Multiplication
    /// saturates for unusually large configured intervals.
    #[must_use]
    pub fn expire_after_seconds(&self) -> u64 {
        self.interval
            .as_secs()
            .saturating_mul(3)
            .max(MINIMUM_EXPIRE_AFTER_SECONDS)
    }
}

/// A serialized MQTT Device Discovery publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryMessage {
    /// MQTT topic on which the retained discovery document is published.
    pub topic: String,
    /// Deterministically serialized JSON discovery document.
    pub payload: String,
}

/// Builds one Home Assistant MQTT Device Discovery publication.
///
/// The caller should publish the returned payload with QoS 1 and the retain
/// flag. The `qos` value inside the document is intentionally zero because it
/// controls Home Assistant's subscription to state and availability updates,
/// not the delivery of the discovery publication itself.
///
/// # Errors
///
/// Returns an error if the discovery document cannot be serialized as JSON.
pub fn build_discovery_message(
    settings: &DiscoverySettings,
) -> Result<DiscoveryMessage, serde_json::Error> {
    let document = DiscoveryDocument {
        device: DeviceInfo {
            identifiers: [&settings.device_id],
            name: &settings.name,
            manufacturer: MANUFACTURER,
            sw_version: SOFTWARE_VERSION,
            model: settings.model.as_deref(),
            serial_number: settings.serial_number.as_deref(),
            hw_version: settings.hardware_version.as_deref(),
        },
        origin: OriginInfo {
            name: ORIGIN_NAME,
            sw_version: SOFTWARE_VERSION,
        },
        state_topic: settings.state_topic(),
        availability_topic: settings.availability_topic(),
        payload_available: ONLINE_PAYLOAD,
        payload_not_available: OFFLINE_PAYLOAD,
        qos: 0,
        components: build_components(settings),
    };

    Ok(DiscoveryMessage {
        topic: settings.discovery_topic(),
        payload: serde_json::to_string(&document)?,
    })
}

#[derive(Serialize)]
struct DiscoveryDocument<'a> {
    device: DeviceInfo<'a>,
    origin: OriginInfo,
    state_topic: String,
    availability_topic: String,
    payload_available: &'static str,
    payload_not_available: &'static str,
    qos: u8,
    components: BTreeMap<&'static str, ComponentConfig>,
}

#[derive(Serialize)]
struct DeviceInfo<'a> {
    identifiers: [&'a str; 1],
    name: &'a str,
    manufacturer: &'static str,
    sw_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    serial_number: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hw_version: Option<&'a str>,
}

#[derive(Serialize)]
struct OriginInfo {
    name: &'static str,
    sw_version: &'static str,
}

#[derive(Clone, Copy)]
enum Platform {
    Sensor,
    BinarySensor,
}

impl Platform {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Sensor => "sensor",
            Self::BinarySensor => "binary_sensor",
        }
    }
}

struct ComponentDefinition {
    key: &'static str,
    platform: Platform,
    name: &'static str,
    enabled_by_default: bool,
    device_class: Option<&'static str>,
    state_class: Option<&'static str>,
    unit_of_measurement: Option<&'static str>,
    suggested_display_precision: Option<u8>,
    options: Option<&'static [&'static str]>,
    payload_on: Option<&'static str>,
    payload_off: Option<&'static str>,
    value_template: &'static str,
}

impl ComponentDefinition {
    fn to_config(&self, settings: &DiscoverySettings, device_slug: &str) -> ComponentConfig {
        ComponentConfig {
            platform: self.platform.as_str(),
            name: self.name,
            unique_id: format!("{}_{}", settings.device_id, self.key),
            default_entity_id: format!("{}.{}_{}", self.platform.as_str(), device_slug, self.key),
            enabled_by_default: self.enabled_by_default,
            entity_category: "diagnostic",
            expire_after: settings.expire_after_seconds(),
            device_class: self.device_class,
            state_class: self.state_class,
            unit_of_measurement: self.unit_of_measurement,
            suggested_display_precision: self.suggested_display_precision,
            options: self.options,
            payload_on: self.payload_on,
            payload_off: self.payload_off,
            value_template: self.value_template,
        }
    }
}

#[derive(Serialize)]
struct ComponentConfig {
    platform: &'static str,
    name: &'static str,
    unique_id: String,
    default_entity_id: String,
    enabled_by_default: bool,
    entity_category: &'static str,
    expire_after: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_class: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_class: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unit_of_measurement: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_display_precision: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<&'static [&'static str]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_on: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_off: Option<&'static str>,
    value_template: &'static str,
}

fn build_components(settings: &DiscoverySettings) -> BTreeMap<&'static str, ComponentConfig> {
    let device_slug = normalize_device_slug(&settings.device_id);
    COMPONENT_DEFINITIONS
        .iter()
        .map(|definition| (definition.key, definition.to_config(settings, &device_slug)))
        .collect()
}

fn normalize_device_slug(device_id: &str) -> String {
    device_id
        .chars()
        .map(|character| match character {
            '-' => '_',
            character => character.to_ascii_lowercase(),
        })
        .collect()
}

fn child_topic(base_topic: &str, child: &str) -> String {
    format!("{}/{}", base_topic.trim_end_matches('/'), child)
}

// Keeping the component table declarative makes the wire contract auditable;
// grouping these independent Home Assistant fields would obscure each row.
#[allow(clippy::too_many_arguments)]
const fn sensor(
    key: &'static str,
    name: &'static str,
    enabled_by_default: bool,
    device_class: Option<&'static str>,
    state_class: Option<&'static str>,
    unit_of_measurement: Option<&'static str>,
    suggested_display_precision: Option<u8>,
    options: Option<&'static [&'static str]>,
    value_template: &'static str,
) -> ComponentDefinition {
    ComponentDefinition {
        key,
        platform: Platform::Sensor,
        name,
        enabled_by_default,
        device_class,
        state_class,
        unit_of_measurement,
        suggested_display_precision,
        options,
        payload_on: None,
        payload_off: None,
        value_template,
    }
}

const fn binary_sensor(
    key: &'static str,
    name: &'static str,
    enabled_by_default: bool,
    value_template: &'static str,
) -> ComponentDefinition {
    ComponentDefinition {
        key,
        platform: Platform::BinarySensor,
        name,
        enabled_by_default,
        device_class: Some("problem"),
        state_class: None,
        unit_of_measurement: None,
        suggested_display_precision: None,
        options: None,
        payload_on: Some("ON"),
        payload_off: Some("OFF"),
        value_template,
    }
}

const COMPONENT_DEFINITIONS: &[ComponentDefinition] = &[
    sensor(
        "health_status",
        "Health status",
        true,
        Some("enum"),
        None,
        None,
        None,
        Some(HEALTH_OPTIONS),
        "{{ value_json.health.status }}",
    ),
    sensor(
        "cpu_temperature",
        "CPU temperature",
        true,
        Some("temperature"),
        Some("measurement"),
        Some("°C"),
        Some(1),
        None,
        "{{ value_json.cpu.temperature_c if value_json.cpu.temperature_c is not none else none }}",
    ),
    sensor(
        "cpu_usage",
        "CPU usage",
        true,
        None,
        Some("measurement"),
        Some("%"),
        Some(1),
        None,
        "{{ value_json.cpu.usage_percent if value_json.cpu.usage_percent is not none else none }}",
    ),
    sensor(
        "load_1",
        "1-minute load average",
        false,
        None,
        Some("measurement"),
        None,
        Some(2),
        None,
        "{{ value_json.cpu.load_1 if value_json.cpu.load_1 is not none else none }}",
    ),
    sensor(
        "load_5",
        "5-minute load average",
        false,
        None,
        Some("measurement"),
        None,
        Some(2),
        None,
        "{{ value_json.cpu.load_5 if value_json.cpu.load_5 is not none else none }}",
    ),
    sensor(
        "load_15",
        "15-minute load average",
        false,
        None,
        Some("measurement"),
        None,
        Some(2),
        None,
        "{{ value_json.cpu.load_15 if value_json.cpu.load_15 is not none else none }}",
    ),
    sensor(
        "cpu_frequency",
        "CPU frequency",
        false,
        Some("frequency"),
        Some("measurement"),
        Some("MHz"),
        Some(1),
        None,
        "{{ value_json.cpu.frequency_mhz if value_json.cpu.frequency_mhz is not none else none }}",
    ),
    sensor(
        "memory_usage",
        "Memory usage",
        true,
        None,
        Some("measurement"),
        Some("%"),
        Some(1),
        None,
        "{{ value_json.memory.used_percent if value_json.memory.used_percent is not none else none }}",
    ),
    sensor(
        "memory_available",
        "Memory available",
        false,
        Some("data_size"),
        Some("measurement"),
        Some("GiB"),
        Some(2),
        None,
        "{{ ((value_json.memory.available_bytes / 1073741824) | round(2)) if value_json.memory.available_bytes is not none else none }}",
    ),
    sensor(
        "swap_usage",
        "Swap usage",
        true,
        None,
        Some("measurement"),
        Some("%"),
        Some(1),
        None,
        "{{ value_json.swap.used_percent if value_json.swap.used_percent is not none else none }}",
    ),
    sensor(
        "swap_used",
        "Swap used",
        false,
        Some("data_size"),
        Some("measurement"),
        Some("MiB"),
        Some(1),
        None,
        "{{ ((value_json.swap.used_bytes / 1048576) | round(1)) if value_json.swap.used_bytes is not none else none }}",
    ),
    sensor(
        "disk_usage",
        "Disk usage",
        true,
        None,
        Some("measurement"),
        Some("%"),
        Some(1),
        None,
        "{{ value_json.disk.used_percent if value_json.disk.used_percent is not none else none }}",
    ),
    sensor(
        "disk_available",
        "Disk available",
        true,
        Some("data_size"),
        Some("measurement"),
        Some("GiB"),
        Some(2),
        None,
        "{{ ((value_json.disk.available_bytes / 1073741824) | round(2)) if value_json.disk.available_bytes is not none else none }}",
    ),
    sensor(
        "uptime",
        "Uptime",
        true,
        Some("duration"),
        Some("measurement"),
        Some("s"),
        Some(0),
        None,
        "{{ value_json.uptime_seconds if value_json.uptime_seconds is not none else none }}",
    ),
    sensor(
        "last_observation",
        "Last observation",
        false,
        Some("timestamp"),
        None,
        None,
        None,
        None,
        "{{ value_json.observed_at }}",
    ),
    sensor(
        "throttled_raw",
        "Raw throttling flags",
        false,
        None,
        None,
        None,
        None,
        None,
        "{{ value_json.power.throttled_raw if value_json.power.throttled_raw is not none else none }}",
    ),
    sensor(
        "collection_duration",
        "Collection duration",
        false,
        Some("duration"),
        Some("measurement"),
        Some("ms"),
        Some(0),
        None,
        "{{ value_json.service.collection_duration_ms }}",
    ),
    sensor(
        "collector_error_count",
        "Collector error count",
        false,
        None,
        Some("measurement"),
        None,
        Some(0),
        None,
        "{{ value_json.health.collector_errors | count }}",
    ),
    binary_sensor(
        "undervoltage_now",
        "Current undervoltage",
        true,
        "{% set v = value_json.power.undervoltage_now %}{{ 'ON' if v is true else ('OFF' if v is false else none) }}",
    ),
    binary_sensor(
        "undervoltage_since_boot",
        "Undervoltage since boot",
        true,
        "{% set v = value_json.power.undervoltage_since_boot %}{{ 'ON' if v is true else ('OFF' if v is false else none) }}",
    ),
    binary_sensor(
        "throttled_now",
        "Current throttling",
        true,
        "{% set v = value_json.power.throttled_now %}{{ 'ON' if v is true else ('OFF' if v is false else none) }}",
    ),
    binary_sensor(
        "throttled_since_boot",
        "Throttling since boot",
        true,
        "{% set v = value_json.power.throttled_since_boot %}{{ 'ON' if v is true else ('OFF' if v is false else none) }}",
    ),
    binary_sensor(
        "arm_frequency_capped_now",
        "ARM frequency capped",
        false,
        "{% set v = value_json.power.arm_frequency_capped_now %}{{ 'ON' if v is true else ('OFF' if v is false else none) }}",
    ),
    binary_sensor(
        "arm_frequency_capped_since_boot",
        "ARM frequency capped since boot",
        false,
        "{% set v = value_json.power.arm_frequency_capped_since_boot %}{{ 'ON' if v is true else ('OFF' if v is false else none) }}",
    ),
    binary_sensor(
        "soft_temperature_limit_now",
        "Soft temperature limit",
        false,
        "{% set v = value_json.power.soft_temperature_limit_now %}{{ 'ON' if v is true else ('OFF' if v is false else none) }}",
    ),
    binary_sensor(
        "soft_temperature_limit_since_boot",
        "Soft temperature limit since boot",
        false,
        "{% set v = value_json.power.soft_temperature_limit_since_boot %}{{ 'ON' if v is true else ('OFF' if v is false else none) }}",
    ),
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{Map, Value};

    use super::*;

    const EXPECTED_COMPONENTS: &[&str] = &[
        "health_status",
        "cpu_temperature",
        "cpu_usage",
        "load_1",
        "load_5",
        "load_15",
        "cpu_frequency",
        "memory_usage",
        "memory_available",
        "swap_usage",
        "swap_used",
        "disk_usage",
        "disk_available",
        "uptime",
        "last_observation",
        "throttled_raw",
        "collection_duration",
        "collector_error_count",
        "undervoltage_now",
        "undervoltage_since_boot",
        "throttled_now",
        "throttled_since_boot",
        "arm_frequency_capped_now",
        "arm_frequency_capped_since_boot",
        "soft_temperature_limit_now",
        "soft_temperature_limit_since_boot",
    ];

    const ENABLED_COMPONENTS: &[&str] = &[
        "health_status",
        "cpu_temperature",
        "cpu_usage",
        "memory_usage",
        "swap_usage",
        "disk_usage",
        "disk_available",
        "uptime",
        "undervoltage_now",
        "undervoltage_since_boot",
        "throttled_now",
        "throttled_since_boot",
    ];

    fn settings(interval_seconds: u64) -> DiscoverySettings {
        DiscoverySettings::new(
            "Lab-Node_7",
            "Laboratory node",
            "building/monitor/Lab-Node_7/",
            "homeassistant/",
            Duration::from_secs(interval_seconds),
        )
    }

    fn document(interval_seconds: u64) -> Value {
        let message = build_discovery_message(&settings(interval_seconds))
            .expect("discovery document should serialize");
        serde_json::from_str(&message.payload).expect("discovery payload should be valid JSON")
    }

    fn components(document: &Value) -> &Map<String, Value> {
        document["components"]
            .as_object()
            .expect("components should be an object")
    }

    #[test]
    fn message_uses_the_device_discovery_topic_and_shared_topics() {
        let settings = settings(30);
        let message = build_discovery_message(&settings).expect("discovery should serialize");
        let payload: Value =
            serde_json::from_str(&message.payload).expect("payload should be JSON");

        assert_eq!(message.topic, "homeassistant/device/Lab-Node_7/config");
        assert_eq!(payload["state_topic"], "building/monitor/Lab-Node_7/state");
        assert_eq!(
            payload["availability_topic"],
            "building/monitor/Lab-Node_7/availability"
        );
        assert_eq!(payload["payload_available"], "online");
        assert_eq!(payload["payload_not_available"], "offline");
        assert_eq!(payload["qos"], 0);
    }

    #[test]
    fn required_metadata_is_present_and_optional_metadata_is_omitted() {
        let payload = document(30);

        assert_eq!(
            payload["device"]["identifiers"],
            serde_json::json!(["Lab-Node_7"])
        );
        assert_eq!(payload["device"]["name"], "Laboratory node");
        assert_eq!(payload["device"]["manufacturer"], "Raspberry Pi");
        assert_eq!(payload["device"]["sw_version"], SOFTWARE_VERSION);
        assert!(payload["device"].get("model").is_none());
        assert!(payload["device"].get("serial_number").is_none());
        assert!(payload["device"].get("hw_version").is_none());
        assert_eq!(payload["origin"]["name"], ORIGIN_NAME);
        assert_eq!(payload["origin"]["sw_version"], SOFTWARE_VERSION);
        assert!(payload["origin"].get("support_url").is_none());
        assert_no_json_null(&payload);
    }

    #[test]
    fn optional_device_metadata_is_emitted_when_available() {
        let settings = settings(30)
            .with_model("Single-board computer")
            .with_serial_number("00000001")
            .with_hardware_version("Revision A");
        let message = build_discovery_message(&settings).expect("discovery should serialize");
        let payload: Value =
            serde_json::from_str(&message.payload).expect("payload should be JSON");

        assert_eq!(payload["device"]["model"], "Single-board computer");
        assert_eq!(payload["device"]["serial_number"], "00000001");
        assert_eq!(payload["device"]["hw_version"], "Revision A");
    }

    #[test]
    fn component_set_and_default_enablement_are_exact() {
        let payload = document(30);
        let components = components(&payload);
        let actual_keys: BTreeSet<_> = components.keys().map(String::as_str).collect();
        let expected_keys: BTreeSet<_> = EXPECTED_COMPONENTS.iter().copied().collect();
        let actual_enabled: BTreeSet<_> = components
            .iter()
            .filter_map(|(key, component)| {
                component["enabled_by_default"]
                    .as_bool()
                    .expect("enablement should be a boolean")
                    .then_some(key.as_str())
            })
            .collect();
        let expected_enabled: BTreeSet<_> = ENABLED_COMPONENTS.iter().copied().collect();

        assert_eq!(components.len(), 26);
        assert_eq!(actual_keys, expected_keys);
        assert_eq!(actual_enabled, expected_enabled);
        assert_eq!(actual_enabled.len(), 12);
        assert_eq!(components.len() - actual_enabled.len(), 14);
    }

    #[test]
    fn every_component_has_stable_unique_and_default_entity_ids() {
        let payload = document(30);
        let components = components(&payload);
        let mut unique_ids = BTreeSet::new();
        let mut default_entity_ids = BTreeSet::new();
        let mut sensor_count = 0;
        let mut binary_sensor_count = 0;

        for (key, component) in components {
            let platform = component["platform"]
                .as_str()
                .expect("platform should be a string");
            match platform {
                "sensor" => sensor_count += 1,
                "binary_sensor" => binary_sensor_count += 1,
                unexpected => panic!("unexpected platform {unexpected}"),
            }

            let unique_id = component["unique_id"]
                .as_str()
                .expect("unique ID should be a string");
            let default_entity_id = component["default_entity_id"]
                .as_str()
                .expect("default entity ID should be a string");
            assert_eq!(unique_id, format!("Lab-Node_7_{key}"));
            assert_eq!(default_entity_id, format!("{platform}.lab_node_7_{key}"));
            assert!(unique_ids.insert(unique_id));
            assert!(default_entity_ids.insert(default_entity_id));
            assert_eq!(component["entity_category"], "diagnostic");
            assert!(component["name"]
                .as_str()
                .is_some_and(|name| !name.is_empty()));
        }

        assert_eq!(sensor_count, 18);
        assert_eq!(binary_sensor_count, 8);
    }

    #[test]
    fn expiry_is_uniform_and_obeys_the_minimum() {
        for (interval, expected_expiry) in [(1, 90), (30, 90), (31, 93), (60, 180)] {
            let payload = document(interval);
            for component in components(&payload).values() {
                assert_eq!(component["expire_after"], expected_expiry);
            }
        }

        let mut settings = settings(1);
        settings.interval = Duration::MAX;
        assert_eq!(settings.expire_after_seconds(), u64::MAX);
    }

    #[test]
    fn nullable_sensor_templates_preserve_unknown_values_and_conversions() {
        let payload = document(30);
        let components = components(&payload);
        let nullable_sensors = [
            "cpu_temperature",
            "cpu_usage",
            "load_1",
            "load_5",
            "load_15",
            "cpu_frequency",
            "memory_usage",
            "memory_available",
            "swap_usage",
            "swap_used",
            "disk_usage",
            "disk_available",
            "uptime",
            "throttled_raw",
        ];

        for key in nullable_sensors {
            let template = components[key]["value_template"]
                .as_str()
                .expect("template should be a string");
            assert!(template.contains(" is not none else none"), "{key}");
        }

        assert_eq!(
            components["memory_available"]["value_template"],
            "{{ ((value_json.memory.available_bytes / 1073741824) | round(2)) if value_json.memory.available_bytes is not none else none }}"
        );
        assert_eq!(
            components["swap_used"]["value_template"],
            "{{ ((value_json.swap.used_bytes / 1048576) | round(1)) if value_json.swap.used_bytes is not none else none }}"
        );
        assert_eq!(
            components["disk_available"]["value_template"],
            "{{ ((value_json.disk.available_bytes / 1073741824) | round(2)) if value_json.disk.available_bytes is not none else none }}"
        );
    }

    #[test]
    fn binary_sensor_templates_preserve_all_three_boolean_states() {
        let payload = document(30);
        for (key, component) in components(&payload) {
            if component["platform"] != "binary_sensor" {
                continue;
            }

            let template = component["value_template"]
                .as_str()
                .expect("template should be a string");
            assert!(template.contains("'ON' if v is true"), "{key}");
            assert!(template.contains("'OFF' if v is false else none"), "{key}");
            assert_eq!(component["payload_on"], "ON");
            assert_eq!(component["payload_off"], "OFF");
            assert_eq!(component["device_class"], "problem");
        }
    }

    #[test]
    fn representative_sensor_metadata_matches_the_state_contract() {
        let payload = document(30);
        let components = components(&payload);

        assert_eq!(components["health_status"]["device_class"], "enum");
        assert_eq!(
            components["health_status"]["options"],
            serde_json::json!(["ok", "warning", "critical", "degraded"])
        );
        assert!(components["health_status"].get("state_class").is_none());

        assert_metadata(
            &components["cpu_temperature"],
            Some("temperature"),
            Some("measurement"),
            Some("°C"),
            Some(1),
        );
        assert_metadata(
            &components["cpu_frequency"],
            Some("frequency"),
            Some("measurement"),
            Some("MHz"),
            Some(1),
        );
        assert_metadata(
            &components["disk_available"],
            Some("data_size"),
            Some("measurement"),
            Some("GiB"),
            Some(2),
        );
        assert_metadata(
            &components["uptime"],
            Some("duration"),
            Some("measurement"),
            Some("s"),
            Some(0),
        );
        assert_metadata(
            &components["last_observation"],
            Some("timestamp"),
            None,
            None,
            None,
        );
    }

    #[test]
    fn non_nullable_templates_follow_the_state_schema() {
        let payload = document(30);
        let components = components(&payload);

        assert_eq!(
            components["health_status"]["value_template"],
            "{{ value_json.health.status }}"
        );
        assert_eq!(
            components["last_observation"]["value_template"],
            "{{ value_json.observed_at }}"
        );
        assert_eq!(
            components["collection_duration"]["value_template"],
            "{{ value_json.service.collection_duration_ms }}"
        );
        assert_eq!(
            components["collector_error_count"]["value_template"],
            "{{ value_json.health.collector_errors | count }}"
        );
    }

    #[test]
    fn serialization_is_deterministic_and_component_keys_are_sorted() {
        let settings = settings(30);
        let first = build_discovery_message(&settings).expect("discovery should serialize");
        let second = build_discovery_message(&settings).expect("discovery should serialize");
        let payload: Value =
            serde_json::from_str(&first.payload).expect("payload should be valid JSON");
        let actual_keys: Vec<_> = components(&payload).keys().map(String::as_str).collect();
        let mut sorted_keys = actual_keys.clone();
        sorted_keys.sort_unstable();

        assert_eq!(first, second);
        assert_eq!(actual_keys, sorted_keys);
    }

    fn assert_metadata(
        component: &Value,
        device_class: Option<&str>,
        state_class: Option<&str>,
        unit: Option<&str>,
        precision: Option<u64>,
    ) {
        assert_eq!(
            component.get("device_class").and_then(Value::as_str),
            device_class
        );
        assert_eq!(
            component.get("state_class").and_then(Value::as_str),
            state_class
        );
        assert_eq!(
            component.get("unit_of_measurement").and_then(Value::as_str),
            unit
        );
        assert_eq!(
            component
                .get("suggested_display_precision")
                .and_then(Value::as_u64),
            precision
        );
    }

    fn assert_no_json_null(value: &Value) {
        match value {
            Value::Null => panic!("optional discovery metadata must be omitted, not null"),
            Value::Array(values) => values.iter().for_each(assert_no_json_null),
            Value::Object(values) => values.values().for_each(assert_no_json_null),
            Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
}
