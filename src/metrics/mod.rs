//! Pure parsers and calculations for Linux and Raspberry Pi health metrics.
//!
//! Keeping parsing separate from file and process I/O makes malformed kernel
//! data recoverable and lets the collector report a missing metric instead of
//! terminating the service.

use std::fmt;

pub mod cpu;
pub mod disk;
pub mod memory;
pub mod metadata;
pub mod power;
pub mod temperature;
pub mod uptime;

/// An error produced while parsing a metric source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    source_name: &'static str,
    detail: String,
}

impl ParseError {
    /// Returns the logical source whose contents could not be parsed.
    #[must_use]
    pub const fn source_name(&self) -> &'static str {
        self.source_name
    }

    /// Returns a human-readable explanation that does not contain credentials.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub(crate) fn new(source_name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            source_name,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "could not parse {}: {}",
            self.source_name, self.detail
        )
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::ParseError;

    #[test]
    fn parse_error_exposes_safe_context() {
        let error = ParseError::new("test metric", "missing value");

        assert_eq!(error.source_name(), "test metric");
        assert_eq!(error.detail(), "missing value");
        assert_eq!(
            error.to_string(),
            "could not parse test metric: missing value"
        );
    }
}
