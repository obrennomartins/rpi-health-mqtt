//! `/proc/uptime` parsing.

use super::ParseError;

const PROC_UPTIME: &str = "/proc/uptime";

/// Parses `/proc/uptime` and returns whole elapsed seconds since boot.
///
/// Fractional seconds are truncated, matching the integer telemetry contract.
pub fn parse_uptime_seconds(input: &str) -> Result<u64, ParseError> {
    let raw = input
        .split_whitespace()
        .next()
        .ok_or_else(|| ParseError::new(PROC_UPTIME, "the uptime value is missing"))?;
    let seconds = raw
        .parse::<f64>()
        .map_err(|_| ParseError::new(PROC_UPTIME, "the uptime value is not a number"))?;
    if !seconds.is_finite() || seconds.is_sign_negative() {
        return Err(ParseError::new(
            PROC_UPTIME,
            "the uptime value must be finite and non-negative",
        ));
    }
    if seconds >= u64::MAX as f64 {
        return Err(ParseError::new(
            PROC_UPTIME,
            "the uptime value exceeds u64 seconds",
        ));
    }
    Ok(seconds.trunc() as u64)
}

#[cfg(test)]
mod tests {
    use super::parse_uptime_seconds;

    #[test]
    fn parses_and_truncates_uptime_seconds() {
        assert_eq!(parse_uptime_seconds("123456.78 987654.32\n"), Ok(123_456));
        assert_eq!(parse_uptime_seconds("0.00 0.00"), Ok(0));
    }

    #[test]
    fn rejects_invalid_uptime() {
        for input in ["", "not-a-number 0", "-1.0 0", "NaN 0", "inf 0"] {
            assert!(
                parse_uptime_seconds(input).is_err(),
                "input should fail: {input}"
            );
        }
    }

    #[test]
    fn rejects_uptime_that_cannot_fit_in_u64() {
        assert!(parse_uptime_seconds("18446744073709551616 0").is_err());
    }
}
