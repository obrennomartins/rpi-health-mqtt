//! `/proc/meminfo` parsing and memory utilization calculations.

use std::collections::BTreeMap;

use super::ParseError;

const PROC_MEMINFO: &str = "/proc/meminfo";
const BYTES_PER_KIBIBYTE: u64 = 1_024;

/// Calculated memory and swap metrics in bytes and percentages.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryMetrics {
    /// Total physical memory in bytes.
    pub total_bytes: u64,
    /// Physical memory readily available to new applications, in bytes.
    pub available_bytes: u64,
    /// Physical memory currently in use, in bytes.
    pub used_bytes: u64,
    /// Percentage of physical memory currently in use.
    pub used_percent: f64,
    /// Total configured swap space in bytes.
    pub swap_total_bytes: u64,
    /// Unused swap space in bytes.
    pub swap_free_bytes: u64,
    /// Swap space currently in use, in bytes.
    pub swap_used_bytes: u64,
    /// Percentage of configured swap space currently in use.
    pub swap_used_percent: f64,
}

/// Parses `/proc/meminfo` and calculates memory and swap utilization.
///
/// Kernels without `MemAvailable` use the documented approximation
/// `MemFree + Buffers + Cached + SReclaimable - Shmem`. The approximation is
/// clamped to `MemTotal` to prevent inconsistent input from underflowing the
/// used-memory calculation.
pub fn parse_meminfo(input: &str) -> Result<MemoryMetrics, ParseError> {
    let values = parse_relevant_values(input)?;
    let total_kib = required(&values, "MemTotal")?;
    if total_kib == 0 {
        return Err(ParseError::new(
            PROC_MEMINFO,
            "MemTotal must be greater than zero",
        ));
    }

    let available_kib = match values.get("MemAvailable").copied() {
        Some(value) => value,
        None => fallback_available_kib(&values)?,
    }
    .min(total_kib);
    let swap_total_kib = required(&values, "SwapTotal")?;
    let swap_free_kib = required(&values, "SwapFree")?.min(swap_total_kib);

    let total_bytes = kib_to_bytes(total_kib, "MemTotal")?;
    let available_bytes = kib_to_bytes(available_kib, "MemAvailable")?;
    let used_bytes = total_bytes
        .checked_sub(available_bytes)
        .ok_or_else(|| ParseError::new(PROC_MEMINFO, "available memory exceeded total memory"))?;
    let swap_total_bytes = kib_to_bytes(swap_total_kib, "SwapTotal")?;
    let swap_free_bytes = kib_to_bytes(swap_free_kib, "SwapFree")?;
    let swap_used_bytes = swap_total_bytes
        .checked_sub(swap_free_bytes)
        .ok_or_else(|| ParseError::new(PROC_MEMINFO, "free swap exceeded total swap"))?;

    Ok(MemoryMetrics {
        total_bytes,
        available_bytes,
        used_bytes,
        used_percent: percentage(used_bytes, total_bytes),
        swap_total_bytes,
        swap_free_bytes,
        swap_used_bytes,
        swap_used_percent: percentage(swap_used_bytes, swap_total_bytes),
    })
}

fn parse_relevant_values(input: &str) -> Result<BTreeMap<&'static str, u64>, ParseError> {
    let mut values = BTreeMap::new();
    for line in input.lines() {
        let Some((raw_key, raw_value)) = line.split_once(':') else {
            continue;
        };
        let Some(key) = canonical_key(raw_key.trim()) else {
            continue;
        };
        if values.contains_key(key) {
            return Err(ParseError::new(
                PROC_MEMINFO,
                format!("{key} appears more than once"),
            ));
        }
        values.insert(key, parse_kib_value(raw_value, key)?);
    }
    Ok(values)
}

fn canonical_key(key: &str) -> Option<&'static str> {
    match key {
        "MemTotal" => Some("MemTotal"),
        "MemAvailable" => Some("MemAvailable"),
        "MemFree" => Some("MemFree"),
        "Buffers" => Some("Buffers"),
        "Cached" => Some("Cached"),
        "SReclaimable" => Some("SReclaimable"),
        "Shmem" => Some("Shmem"),
        "SwapTotal" => Some("SwapTotal"),
        "SwapFree" => Some("SwapFree"),
        _ => None,
    }
}

fn parse_kib_value(raw: &str, key: &'static str) -> Result<u64, ParseError> {
    let mut fields = raw.split_whitespace();
    let value = fields
        .next()
        .ok_or_else(|| ParseError::new(PROC_MEMINFO, format!("{key} has no value")))?
        .parse::<u64>()
        .map_err(|_| ParseError::new(PROC_MEMINFO, format!("{key} is not an unsigned integer")))?;
    if fields.next() != Some("kB") || fields.next().is_some() {
        return Err(ParseError::new(
            PROC_MEMINFO,
            format!("{key} must use the kB unit"),
        ));
    }
    Ok(value)
}

fn required(values: &BTreeMap<&'static str, u64>, key: &'static str) -> Result<u64, ParseError> {
    values
        .get(key)
        .copied()
        .ok_or_else(|| ParseError::new(PROC_MEMINFO, format!("{key} is missing")))
}

fn fallback_available_kib(values: &BTreeMap<&'static str, u64>) -> Result<u64, ParseError> {
    let free = required(values, "MemFree")?;
    let buffers = required(values, "Buffers")?;
    let cached = required(values, "Cached")?;
    let reclaimable = required(values, "SReclaimable")?;
    let shared = required(values, "Shmem")?;

    checked_kib_sum(&[free, buffers, cached, reclaimable]).map(|sum| sum.saturating_sub(shared))
}

fn checked_kib_sum(values: &[u64]) -> Result<u64, ParseError> {
    values.iter().try_fold(0_u64, |total, value| {
        total.checked_add(*value).ok_or_else(|| {
            ParseError::new(PROC_MEMINFO, "the available-memory fallback overflowed u64")
        })
    })
}

fn kib_to_bytes(value: u64, key: &'static str) -> Result<u64, ParseError> {
    value
        .checked_mul(BYTES_PER_KIBIBYTE)
        .ok_or_else(|| ParseError::new(PROC_MEMINFO, format!("{key} overflows bytes as u64")))
}

fn percentage(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        100.0 * used as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::parse_meminfo;

    const MEMINFO: &str = include_str!("../../tests/fixtures/proc_meminfo.txt");

    #[test]
    fn uses_mem_available_when_the_kernel_reports_it() {
        let metrics = parse_meminfo(MEMINFO).expect("fixture should be valid");

        assert_eq!(metrics.total_bytes, 1_024_000);
        assert_eq!(metrics.available_bytes, 409_600);
        assert_eq!(metrics.used_bytes, 614_400);
        assert_eq!(metrics.used_percent, 60.0);
        assert_eq!(metrics.swap_total_bytes, 512_000);
        assert_eq!(metrics.swap_free_bytes, 204_800);
        assert_eq!(metrics.swap_used_bytes, 307_200);
        assert_eq!(metrics.swap_used_percent, 60.0);
    }

    #[test]
    fn calculates_available_memory_with_the_legacy_fallback() {
        let input = "\
MemTotal:       1000 kB\n\
MemFree:         100 kB\n\
Buffers:          50 kB\n\
Cached:          300 kB\n\
SReclaimable:     80 kB\n\
Shmem:            30 kB\n\
SwapTotal:         0 kB\n\
SwapFree:          0 kB\n";

        let metrics = parse_meminfo(input).expect("fallback fields should be valid");
        assert_eq!(metrics.available_bytes, 500 * 1_024);
        assert_eq!(metrics.used_bytes, 500 * 1_024);
        assert_eq!(metrics.used_percent, 50.0);
    }

    #[test]
    fn reports_zero_percent_when_swap_is_not_configured() {
        let input = MEMINFO
            .replace("SwapTotal:         500 kB", "SwapTotal:           0 kB")
            .replace("SwapFree:          200 kB", "SwapFree:            0 kB");

        let metrics = parse_meminfo(&input).expect("zero swap should be valid");
        assert_eq!(metrics.swap_used_bytes, 0);
        assert_eq!(metrics.swap_used_percent, 0.0);
    }

    #[test]
    fn clamps_transient_free_values_to_their_totals() {
        let input = "\
MemTotal:       1000 kB\n\
MemAvailable:   1001 kB\n\
SwapTotal:       100 kB\n\
SwapFree:        101 kB\n";

        let metrics = parse_meminfo(input).expect("input should remain safe");
        assert_eq!(metrics.used_bytes, 0);
        assert_eq!(metrics.swap_used_bytes, 0);
    }

    #[test]
    fn supports_values_larger_than_a_32_bit_address_space() {
        let input = "\
MemTotal:       5000000000 kB\n\
MemAvailable:   4000000000 kB\n\
SwapTotal:      1000000000 kB\n\
SwapFree:        500000000 kB\n";

        let metrics = parse_meminfo(input).expect("large u64 values should be valid");
        assert_eq!(metrics.total_bytes, 5_120_000_000_000);
        assert_eq!(metrics.used_bytes, 1_024_000_000_000);
    }

    #[test]
    fn rejects_missing_or_malformed_required_values() {
        assert!(parse_meminfo("SwapTotal: 0 kB\nSwapFree: 0 kB\n").is_err());
        assert!(parse_meminfo("MemTotal: many kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n").is_err());
        assert!(parse_meminfo("MemTotal: 10 MB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n").is_err());
    }

    #[test]
    fn rejects_an_incomplete_legacy_fallback() {
        let input = "MemTotal: 100 kB\nMemFree: 20 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n";

        assert!(parse_meminfo(input).is_err());
    }

    #[test]
    fn rejects_byte_conversion_overflow() {
        let input = format!(
            "MemTotal: {} kB\nMemAvailable: 0 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n",
            u64::MAX
        );

        assert!(parse_meminfo(&input).is_err());
    }
}
