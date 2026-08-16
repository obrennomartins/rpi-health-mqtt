//! CPU temperature parsers for sysfs and the firmware command fallback.

use super::ParseError;

const SYSFS_TEMPERATURE: &str = "CPU thermal zone";
const FIRMWARE_TEMPERATURE: &str = "firmware temperature";

/// Parses a sysfs temperature expressed in thousandths of a degree Celsius.
///
/// Negative sensor readings are accepted because the kernel interface uses a
/// signed value and sub-zero operation is possible in some environments.
pub fn parse_millidegrees_celsius(input: &str) -> Result<f64, ParseError> {
    let mut fields = input.split_whitespace();
    let raw = fields
        .next()
        .ok_or_else(|| ParseError::new(SYSFS_TEMPERATURE, "the value is missing"))?;
    if fields.next().is_some() {
        return Err(ParseError::new(
            SYSFS_TEMPERATURE,
            "the value contains unexpected fields",
        ));
    }
    let millidegrees = raw
        .parse::<i64>()
        .map_err(|_| ParseError::new(SYSFS_TEMPERATURE, "the value is not a signed integer"))?;
    Ok(millidegrees as f64 / 1_000.0)
}

/// Parses the output of `vcgencmd measure_temp` as degrees Celsius.
pub fn parse_vcgencmd_temperature(input: &str) -> Result<f64, ParseError> {
    let (key, raw_value) = input
        .trim()
        .split_once('=')
        .ok_or_else(|| ParseError::new(FIRMWARE_TEMPERATURE, "the assignment is missing"))?;
    if key.trim() != "temp" {
        return Err(ParseError::new(
            FIRMWARE_TEMPERATURE,
            "the expected temp key is missing",
        ));
    }

    let raw_value = raw_value.trim();
    let numeric = raw_value
        .strip_suffix("'C")
        .or_else(|| raw_value.strip_suffix("°C"))
        .ok_or_else(|| ParseError::new(FIRMWARE_TEMPERATURE, "the Celsius unit suffix is missing"))?
        .trim();
    let value = numeric
        .parse::<f64>()
        .map_err(|_| ParseError::new(FIRMWARE_TEMPERATURE, "the value is not a number"))?;
    if !value.is_finite() {
        return Err(ParseError::new(
            FIRMWARE_TEMPERATURE,
            "the value must be finite",
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{parse_millidegrees_celsius, parse_vcgencmd_temperature};

    #[test]
    fn converts_millidegrees_to_degrees_celsius() {
        assert_eq!(parse_millidegrees_celsius("49875\n"), Ok(49.875));
        assert_eq!(parse_millidegrees_celsius("-1250"), Ok(-1.25));
    }

    #[test]
    fn rejects_malformed_millidegree_values() {
        assert!(parse_millidegrees_celsius("").is_err());
        assert!(parse_millidegrees_celsius("49.5").is_err());
        assert!(parse_millidegrees_celsius("49000 mC").is_err());
    }

    #[test]
    fn parses_firmware_temperature_output() {
        assert_eq!(parse_vcgencmd_temperature("temp=49.8'C\n"), Ok(49.8));
        assert_eq!(parse_vcgencmd_temperature(" temp = -1.25°C \n"), Ok(-1.25));
    }

    #[test]
    fn rejects_malformed_firmware_temperature_output() {
        for input in [
            "",
            "temperature=49.8'C",
            "temp=49.8",
            "temp=NaN'C",
            "temp=inf'C",
        ] {
            assert!(
                parse_vcgencmd_temperature(input).is_err(),
                "input should fail: {input}"
            );
        }
    }
}
