//! Parser for Raspberry Pi firmware throttling and power-health flags.

use super::ParseError;

const THROTTLED_OUTPUT: &str = "vcgencmd get_throttled";

/// Decoded power, throttling, and thermal-limit flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThrottledStatus {
    /// Raw firmware bitmask.
    pub raw: u32,
    /// Whether undervoltage is currently detected.
    pub undervoltage_now: bool,
    /// Whether the ARM frequency is currently capped.
    pub arm_frequency_capped_now: bool,
    /// Whether throttling is currently active.
    pub throttled_now: bool,
    /// Whether the soft temperature limit is currently active.
    pub soft_temperature_limit_now: bool,
    /// Whether undervoltage has occurred since boot.
    pub undervoltage_since_boot: bool,
    /// Whether ARM frequency capping has occurred since boot.
    pub arm_frequency_capped_since_boot: bool,
    /// Whether throttling has occurred since boot.
    pub throttled_since_boot: bool,
    /// Whether the soft temperature limit has occurred since boot.
    pub soft_temperature_limit_since_boot: bool,
}

impl ThrottledStatus {
    /// Formats the raw mask as normalized lowercase hexadecimal.
    #[must_use]
    pub fn raw_hex(&self) -> String {
        format!("0x{:x}", self.raw)
    }
}

/// Parses and decodes the output of `vcgencmd get_throttled`.
pub fn parse_throttled(input: &str) -> Result<ThrottledStatus, ParseError> {
    let (key, raw_value) = input
        .trim()
        .split_once('=')
        .ok_or_else(|| ParseError::new(THROTTLED_OUTPUT, "the assignment is missing"))?;
    if key.trim() != "throttled" {
        return Err(ParseError::new(
            THROTTLED_OUTPUT,
            "the expected throttled key is missing",
        ));
    }

    let raw_value = raw_value.trim();
    let hexadecimal = raw_value
        .strip_prefix("0x")
        .or_else(|| raw_value.strip_prefix("0X"))
        .ok_or_else(|| ParseError::new(THROTTLED_OUTPUT, "the value must have a 0x prefix"))?;
    if hexadecimal.is_empty() {
        return Err(ParseError::new(
            THROTTLED_OUTPUT,
            "the hexadecimal value is empty",
        ));
    }
    let raw = u32::from_str_radix(hexadecimal, 16).map_err(|_| {
        ParseError::new(
            THROTTLED_OUTPUT,
            "the value is not a 32-bit hexadecimal mask",
        )
    })?;

    Ok(ThrottledStatus {
        raw,
        undervoltage_now: has_bit(raw, 0),
        arm_frequency_capped_now: has_bit(raw, 1),
        throttled_now: has_bit(raw, 2),
        soft_temperature_limit_now: has_bit(raw, 3),
        undervoltage_since_boot: has_bit(raw, 16),
        arm_frequency_capped_since_boot: has_bit(raw, 17),
        throttled_since_boot: has_bit(raw, 18),
        soft_temperature_limit_since_boot: has_bit(raw, 19),
    })
}

const fn has_bit(value: u32, bit: u32) -> bool {
    value & (1_u32 << bit) != 0
}

#[cfg(test)]
mod tests {
    use super::{parse_throttled, ThrottledStatus};

    #[test]
    fn decodes_required_firmware_samples() {
        let cases = [
            ("throttled=0x0", 0_u32),
            ("throttled=0x1", 0x1),
            ("throttled=0x10000", 0x1_0000),
            ("throttled=0x50005", 0x5_0005),
            ("throttled=0xA000A", 0xA_000A),
        ];

        for (input, raw) in cases {
            assert_eq!(
                parse_throttled(input).expect("sample should be valid").raw,
                raw
            );
        }
    }

    #[test]
    fn accepts_spaces_and_a_trailing_line_break() {
        let parsed = parse_throttled("  throttled = 0X50005  \n")
            .expect("whitespace and uppercase prefix should be accepted");

        assert_eq!(parsed.raw_hex(), "0x50005");
        assert!(parsed.undervoltage_now);
        assert!(parsed.throttled_now);
        assert!(parsed.undervoltage_since_boot);
        assert!(parsed.throttled_since_boot);
        assert!(!parsed.arm_frequency_capped_now);
        assert!(!parsed.soft_temperature_limit_now);
        assert!(!parsed.arm_frequency_capped_since_boot);
        assert!(!parsed.soft_temperature_limit_since_boot);
    }

    #[test]
    fn distinguishes_current_and_historical_flags() {
        let current = parse_throttled("throttled=0x1").expect("sample should be valid");
        let historical = parse_throttled("throttled=0x10000").expect("sample should be valid");

        assert!(current.undervoltage_now);
        assert!(!current.undervoltage_since_boot);
        assert!(!historical.undervoltage_now);
        assert!(historical.undervoltage_since_boot);
    }

    #[test]
    fn zero_mask_sets_every_flag_to_false() {
        assert_eq!(
            parse_throttled("throttled=0x0"),
            Ok(ThrottledStatus {
                raw: 0,
                undervoltage_now: false,
                arm_frequency_capped_now: false,
                throttled_now: false,
                soft_temperature_limit_now: false,
                undervoltage_since_boot: false,
                arm_frequency_capped_since_boot: false,
                throttled_since_boot: false,
                soft_temperature_limit_since_boot: false,
            })
        );
    }

    #[test]
    fn rejects_malformed_or_out_of_range_masks() {
        for input in [
            "",
            "value=0x1",
            "throttled=1",
            "throttled=0x",
            "throttled=0xGG",
            "throttled=0x100000000",
            "throttled=0x1 extra",
        ] {
            assert!(
                parse_throttled(input).is_err(),
                "input should fail: {input}"
            );
        }
    }
}
