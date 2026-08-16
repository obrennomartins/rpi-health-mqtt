//! Strict, secret-safe service configuration.

use std::{
    fmt, fs,
    num::NonZeroU16,
    path::{Path, PathBuf},
    time::Duration,
};

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use thiserror::Error;

const MAX_CONFIG_BYTES: u64 = 65_536;
const MAX_PASSWORD_BYTES: u64 = 65_536;

/// A validated service configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    device: DeviceConfig,
    mqtt: MqttConfig,
    collector: CollectorConfig,
}

impl Config {
    /// Loads and validates a TOML configuration file.
    ///
    /// # Errors
    ///
    /// Returns a sanitized error for inaccessible, oversized, malformed, or
    /// invalid configuration. TOML source values are never repeated in errors.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let metadata = fs::metadata(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(ConfigError::NotAFile(path.to_path_buf()));
        }
        if metadata.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::TooLarge(path.to_path_buf()));
        }
        let source = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&source)
    }

    /// Parses and validates TOML without reading the credential file.
    ///
    /// # Errors
    ///
    /// Returns a sanitized syntax or field-validation error.
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        if source.len() as u64 > MAX_CONFIG_BYTES {
            return Err(ConfigError::SourceTooLarge);
        }
        let raw: RawConfig = toml::from_str(source)
            .map_err(|error| ConfigError::Toml(sanitize_toml_error(&error)))?;
        raw.validate()
    }

    /// Returns the device identity configuration.
    #[must_use]
    pub fn device(&self) -> &DeviceConfig {
        &self.device
    }

    /// Returns the MQTT configuration.
    #[must_use]
    pub fn mqtt(&self) -> &MqttConfig {
        &self.mqtt
    }

    /// Returns the collection configuration.
    #[must_use]
    pub fn collector(&self) -> &CollectorConfig {
        &self.collector
    }
}

/// Validated public device identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceConfig {
    id: String,
    name: String,
}

impl DeviceConfig {
    /// Returns the stable, configured device identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Returns the human-friendly device name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Validated MQTT connection and publication settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MqttConfig {
    host: String,
    port: NonZeroU16,
    client_id: String,
    username: String,
    password_file: PathBuf,
    base_topic: String,
    discovery_prefix: String,
    keep_alive: Duration,
}

impl MqttConfig {
    /// Returns the broker host.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
    /// Returns the broker TCP port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port.get()
    }
    /// Returns the MQTT client identifier.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
    /// Returns the configured username.
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }
    /// Returns the credential-file path.
    #[must_use]
    pub fn password_file(&self) -> &Path {
        &self.password_file
    }
    /// Returns the publication topic root.
    #[must_use]
    pub fn base_topic(&self) -> &str {
        &self.base_topic
    }
    /// Returns the Home Assistant discovery topic prefix.
    #[must_use]
    pub fn discovery_prefix(&self) -> &str {
        &self.discovery_prefix
    }
    /// Returns the MQTT keep-alive duration.
    #[must_use]
    pub fn keep_alive(&self) -> Duration {
        self.keep_alive
    }
    /// Returns the state publication topic.
    #[must_use]
    pub fn state_topic(&self) -> String {
        format!("{}/state", self.base_topic)
    }
    /// Returns the availability publication topic.
    #[must_use]
    pub fn availability_topic(&self) -> String {
        format!("{}/availability", self.base_topic)
    }
    /// Returns the discovery topic for `device_id`.
    #[must_use]
    pub fn discovery_topic(&self, device_id: &str) -> String {
        format!("{}/device/{device_id}/config", self.discovery_prefix)
    }
}

/// Validated collection scheduling and source settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectorConfig {
    interval: Duration,
    root_filesystem: PathBuf,
    vcgencmd_path: PathBuf,
    command_timeout: Duration,
}

impl CollectorConfig {
    /// Returns the collection interval.
    #[must_use]
    pub fn interval(&self) -> Duration {
        self.interval
    }
    /// Returns the filesystem mount point to measure.
    #[must_use]
    pub fn root_filesystem(&self) -> &Path {
        &self.root_filesystem
    }
    /// Returns the configured firmware command path.
    #[must_use]
    pub fn vcgencmd_path(&self) -> &Path {
        &self.vcgencmd_path
    }
    /// Returns the firmware-command deadline.
    #[must_use]
    pub fn command_timeout(&self) -> Duration {
        self.command_timeout
    }
}

/// MQTT credentials whose secret is redacted from debug output.
pub struct MqttCredentials {
    username: String,
    password: SecretString,
}

impl MqttCredentials {
    /// Loads credentials from the validated password-file path.
    ///
    /// Exactly one final LF or CRLF is removed; all other whitespace is kept.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, unsafe, oversized, non-UTF-8, or empty files.
    pub fn load(config: &MqttConfig) -> Result<Self, ConfigError> {
        let path = config.password_file();
        let metadata = fs::metadata(path).map_err(|source| ConfigError::CredentialRead {
            path: path.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(ConfigError::CredentialNotAFile(path.to_path_buf()));
        }
        if metadata.len() > MAX_PASSWORD_BYTES {
            return Err(ConfigError::CredentialTooLarge(path.to_path_buf()));
        }
        validate_secret_permissions(path, &metadata)?;
        let bytes = fs::read(path).map_err(|source| ConfigError::CredentialRead {
            path: path.to_path_buf(),
            source,
        })?;
        let mut password = String::from_utf8(bytes)
            .map_err(|_| ConfigError::CredentialEncoding(path.to_path_buf()))?;
        if password.ends_with("\r\n") {
            password.truncate(password.len() - 2);
        } else if password.ends_with('\n') {
            password.pop();
        }
        if password.is_empty() {
            return Err(ConfigError::CredentialEmpty(path.to_path_buf()));
        }
        Ok(Self {
            username: config.username.clone(),
            password: SecretString::from(password.into_boxed_str()),
        })
    }

    /// Returns the MQTT username.
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Exposes the password only to the MQTT connection builder.
    #[must_use]
    pub fn expose_password(&self) -> &str {
        self.password.expose_secret()
    }
}

impl fmt::Debug for MqttCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MqttCredentials")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// Sanitized configuration failures.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The configuration file could not be read.
    #[error("cannot read configuration file {path}: {source}")]
    Read {
        /// Configuration path that could not be read.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The configuration path is not a regular file.
    #[error("configuration path is not a regular file: {0}")]
    NotAFile(PathBuf),
    /// The configuration file exceeds the size limit.
    #[error("configuration file exceeds 64 KiB: {0}")]
    TooLarge(PathBuf),
    /// In-memory TOML exceeds the size limit.
    #[error("configuration source exceeds 64 KiB")]
    SourceTooLarge,
    /// TOML syntax or structure is invalid.
    #[error("invalid TOML: {0}")]
    Toml(String),
    /// A field failed static validation.
    #[error("invalid {field}: {reason}")]
    InvalidField {
        /// Stable dotted name of the invalid field.
        field: &'static str,
        /// Non-sensitive description of the validation rule.
        reason: &'static str,
    },
    /// The credential file could not be read.
    #[error("cannot read credential file {path}: {source}")]
    CredentialRead {
        /// Credential path that could not be read.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The credential path is not a regular file.
    #[error("credential path is not a regular file: {0}")]
    CredentialNotAFile(PathBuf),
    /// The credential file exceeds the size limit.
    #[error("credential file exceeds 64 KiB: {0}")]
    CredentialTooLarge(PathBuf),
    /// The credential file is not valid UTF-8.
    #[error("credential file is not valid UTF-8: {0}")]
    CredentialEncoding(PathBuf),
    /// The credential becomes empty after removing its final line ending.
    #[error("credential file is empty: {0}")]
    CredentialEmpty(PathBuf),
    /// The credential file has unsafe Unix permissions.
    #[error("credential file permissions are too broad: {0}")]
    CredentialPermissions(PathBuf),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    device: RawDevice,
    mqtt: RawMqtt,
    #[serde(default)]
    collector: RawCollector,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDevice {
    id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMqtt {
    #[serde(default = "default_host")]
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    client_id: String,
    username: String,
    password_file: PathBuf,
    base_topic: String,
    #[serde(default = "default_discovery_prefix")]
    discovery_prefix: String,
    #[serde(default = "default_keep_alive")]
    keep_alive_seconds: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCollector {
    #[serde(default = "default_interval")]
    interval_seconds: u64,
    #[serde(default = "default_root")]
    root_filesystem: PathBuf,
    #[serde(default = "default_vcgencmd")]
    vcgencmd_path: PathBuf,
    #[serde(default = "default_command_timeout")]
    command_timeout_seconds: u64,
}

impl Default for RawCollector {
    fn default() -> Self {
        Self {
            interval_seconds: default_interval(),
            root_filesystem: default_root(),
            vcgencmd_path: default_vcgencmd(),
            command_timeout_seconds: default_command_timeout(),
        }
    }
}

impl RawConfig {
    fn validate(self) -> Result<Config, ConfigError> {
        validate_identifier("device.id", &self.device.id, 64, b"_-")?;
        validate_text("device.name", &self.device.name, 128)?;
        validate_host(&self.mqtt.host)?;
        let port = NonZeroU16::new(self.mqtt.port).ok_or(ConfigError::InvalidField {
            field: "mqtt.port",
            reason: "must be between 1 and 65535",
        })?;
        validate_identifier("mqtt.client_id", &self.mqtt.client_id, 128, b"._-")?;
        validate_text("mqtt.username", &self.mqtt.username, 256)?;
        validate_absolute("mqtt.password_file", &self.mqtt.password_file)?;
        validate_topic("mqtt.base_topic", &self.mqtt.base_topic)?;
        validate_topic("mqtt.discovery_prefix", &self.mqtt.discovery_prefix)?;
        validate_seconds(
            "mqtt.keep_alive_seconds",
            self.mqtt.keep_alive_seconds,
            5,
            3_600,
        )?;
        validate_seconds(
            "collector.interval_seconds",
            self.collector.interval_seconds,
            5,
            86_400,
        )?;
        validate_seconds(
            "collector.command_timeout_seconds",
            self.collector.command_timeout_seconds,
            1,
            30,
        )?;
        if self.collector.command_timeout_seconds >= self.collector.interval_seconds {
            return Err(ConfigError::InvalidField {
                field: "collector.command_timeout_seconds",
                reason: "must be shorter than collector.interval_seconds",
            });
        }
        validate_absolute("collector.root_filesystem", &self.collector.root_filesystem)?;
        validate_absolute("collector.vcgencmd_path", &self.collector.vcgencmd_path)?;
        let derived = [
            format!("{}/state", self.mqtt.base_topic),
            format!("{}/availability", self.mqtt.base_topic),
            format!(
                "{}/device/{}/config",
                self.mqtt.discovery_prefix, self.device.id
            ),
        ];
        if derived.iter().any(|topic| topic.len() > 65_535) {
            return Err(ConfigError::InvalidField {
                field: "mqtt topics",
                reason: "a derived topic exceeds 65535 bytes",
            });
        }
        Ok(Config {
            device: DeviceConfig {
                id: self.device.id,
                name: self.device.name,
            },
            mqtt: MqttConfig {
                host: self.mqtt.host,
                port,
                client_id: self.mqtt.client_id,
                username: self.mqtt.username,
                password_file: self.mqtt.password_file,
                base_topic: self.mqtt.base_topic,
                discovery_prefix: self.mqtt.discovery_prefix,
                keep_alive: Duration::from_secs(self.mqtt.keep_alive_seconds),
            },
            collector: CollectorConfig {
                interval: Duration::from_secs(self.collector.interval_seconds),
                root_filesystem: self.collector.root_filesystem,
                vcgencmd_path: self.collector.vcgencmd_path,
                command_timeout: Duration::from_secs(self.collector.command_timeout_seconds),
            },
        })
    }
}

fn validate_identifier(
    field: &'static str,
    value: &str,
    maximum: usize,
    punctuation: &[u8],
) -> Result<(), ConfigError> {
    if value.is_empty()
        || value.len() > maximum
        || !value.is_ascii()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || punctuation.contains(&byte))
    {
        return Err(ConfigError::InvalidField {
            field,
            reason: "contains unsupported characters or has an invalid length",
        });
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<(), ConfigError> {
    if value.is_empty()
        || value.len() > maximum
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ConfigError::InvalidField {
            field,
            reason: "must be non-empty, trimmed, and free of control characters",
        });
    }
    Ok(())
}

fn validate_host(value: &str) -> Result<(), ConfigError> {
    if value.is_empty()
        || value.len() > 253
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
        || value
            .chars()
            .any(|character| matches!(character, '/' | '@' | '?' | '#'))
        || value.contains("://")
    {
        return Err(ConfigError::InvalidField {
            field: "mqtt.host",
            reason: "must be a host name or address without credentials or a URI scheme",
        });
    }
    Ok(())
}

fn validate_topic(field: &'static str, value: &str) -> Result<(), ConfigError> {
    if value.is_empty()
        || value.len() > 65_535
        || value.trim() != value
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains("//")
        || value
            .chars()
            .any(|character| matches!(character, '+' | '#'))
        || value.chars().any(char::is_control)
    {
        return Err(ConfigError::InvalidField {
            field,
            reason: "must be a concrete MQTT topic without empty levels or wildcards",
        });
    }
    Ok(())
}

fn validate_seconds(
    field: &'static str,
    value: u64,
    minimum: u64,
    maximum: u64,
) -> Result<(), ConfigError> {
    if !(minimum..=maximum).contains(&value) {
        return Err(ConfigError::InvalidField {
            field,
            reason: "is outside the allowed range",
        });
    }
    Ok(())
}

fn validate_absolute(field: &'static str, value: &Path) -> Result<(), ConfigError> {
    if !value.is_absolute() {
        return Err(ConfigError::InvalidField {
            field,
            reason: "must be an absolute path",
        });
    }
    Ok(())
}

fn sanitize_toml_error(error: &toml::de::Error) -> String {
    match error.span() {
        Some(span) => format!("{} at byte {}", error.message(), span.start),
        None => error.message().to_owned(),
    }
}

#[cfg(unix)]
fn validate_secret_permissions(path: &Path, metadata: &fs::Metadata) -> Result<(), ConfigError> {
    use std::os::unix::fs::MetadataExt;
    if metadata.mode() & 0o026 != 0 {
        return Err(ConfigError::CredentialPermissions(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_secret_permissions(_path: &Path, _metadata: &fs::Metadata) -> Result<(), ConfigError> {
    Ok(())
}

fn default_host() -> String {
    "127.0.0.1".to_owned()
}
fn default_port() -> u16 {
    1883
}
fn default_discovery_prefix() -> String {
    "homeassistant".to_owned()
}
fn default_keep_alive() -> u64 {
    30
}
fn default_interval() -> u64 {
    30
}
fn default_root() -> PathBuf {
    PathBuf::from("/")
}
fn default_vcgencmd() -> PathBuf {
    PathBuf::from("/usr/bin/vcgencmd")
}
fn default_command_timeout() -> u64 {
    2
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    const VALID: &str = r#"
[device]
id = "example-pi"
name = "Example Raspberry Pi"

[mqtt]
client_id = "rpi-health-mqtt-example-pi"
username = "monitor-example"
password_file = "/etc/rpi-health-mqtt/mqtt-password"
base_topic = "example/monitor/example-pi"
"#;

    #[test]
    fn valid_minimal_config_applies_safe_defaults_and_topics() {
        let config = Config::parse(VALID).expect("configuration should be valid");
        assert_eq!(config.device().id(), "example-pi");
        assert_eq!(config.mqtt().host(), "127.0.0.1");
        assert_eq!(config.mqtt().port(), 1883);
        assert_eq!(
            config.mqtt().state_topic(),
            "example/monitor/example-pi/state"
        );
        assert_eq!(
            config.mqtt().availability_topic(),
            "example/monitor/example-pi/availability"
        );
        assert_eq!(
            config.mqtt().discovery_topic(config.device().id()),
            "homeassistant/device/example-pi/config"
        );
        assert_eq!(config.collector().interval(), Duration::from_secs(30));
    }

    #[test]
    fn rejects_unknown_inline_secret_and_sanitizes_its_value() {
        let sentinel = "never-repeat-this-value";
        let source = VALID.replace(
            "username = \"monitor-example\"",
            &format!("username = \"monitor-example\"\npassword = \"{sentinel}\""),
        );
        let error = Config::parse(&source)
            .expect_err("unknown password must fail")
            .to_string();
        assert!(!error.contains(sentinel));
        assert!(error.contains("unknown field"));
    }

    #[test]
    fn validates_identifier_topic_duration_and_paths() {
        for (needle, replacement) in [
            ("id = \"example-pi\"", "id = \"bad/id\""),
            (
                "base_topic = \"example/monitor/example-pi\"",
                "base_topic = \"example/+/state\"",
            ),
            (
                "password_file = \"/etc/rpi-health-mqtt/mqtt-password\"",
                "password_file = \"relative-password\"",
            ),
        ] {
            assert!(Config::parse(&VALID.replace(needle, replacement)).is_err());
        }
        let source = format!("{VALID}\n[collector]\ninterval_seconds = 4\n");
        assert!(Config::parse(&source).is_err());
        let source =
            format!("{VALID}\n[collector]\ninterval_seconds = 5\ncommand_timeout_seconds = 5\n");
        assert!(Config::parse(&source).is_err());
    }

    #[test]
    fn rejects_unknown_sections_duplicate_fields_and_oversized_source() {
        assert!(Config::parse(&format!("{VALID}\n[unknown]\nvalue = 1\n")).is_err());
        assert!(Config::parse(&VALID.replace(
            "username = \"monitor-example\"",
            "username = \"one\"\nusername = \"two\""
        ))
        .is_err());
        assert!(matches!(
            Config::parse(&"x".repeat(65_537)),
            Err(ConfigError::SourceTooLarge)
        ));
    }

    #[test]
    fn credential_debug_is_redacted_and_one_line_ending_is_removed() {
        let directory = temporary_directory();
        fs::create_dir_all(&directory).expect("temporary directory should be created");
        let password_path = directory.join("password");
        let sentinel = "fixture-secret\nkept";
        fs::write(&password_path, format!("{sentinel}\r\n")).expect("fixture should be written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&password_path, fs::Permissions::from_mode(0o640))
                .expect("fixture permissions should be restricted");
        }
        let source = VALID.replace(
            "/etc/rpi-health-mqtt/mqtt-password",
            password_path.to_str().expect("path is UTF-8"),
        );
        let config = Config::parse(&source).expect("configuration should be valid");
        let credentials = MqttCredentials::load(config.mqtt()).expect("credentials should load");
        assert_eq!(credentials.expose_password(), sentinel);
        let debug = format!("{credentials:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(sentinel));
        fs::remove_dir_all(directory).expect("temporary directory should be removed");
    }

    fn temporary_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rpi-health-mqtt-config-test-{}-{nonce}",
            std::process::id()
        ))
    }
}
