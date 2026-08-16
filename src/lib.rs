//! Raspberry Pi health monitoring and MQTT publication primitives.
//!
//! The library contains the testable implementation used by the
//! `rpi-health-mqtt` service binary.

#![forbid(unsafe_code)]

/// The service version embedded at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn package_version_is_available() {
        assert!(!VERSION.is_empty());
    }
}
