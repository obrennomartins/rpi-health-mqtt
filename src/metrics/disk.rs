//! Checked filesystem utilization calculations for `statvfs` data.

use crate::model::DiskMetrics;

use super::ParseError;

const STATVFS: &str = "statvfs";

/// Filesystem block counters returned by the operating system.
///
/// The collector converts platform-specific `statvfs` fields into these
/// fixed-width values before performing any arithmetic. This keeps large
/// filesystems safe when the service is compiled for a 32-bit target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatvfsValues {
    /// Preferred block size for filesystem size calculations.
    pub fragment_size: u64,
    /// Total number of data blocks.
    pub blocks: u64,
    /// Number of free blocks, including blocks reserved for privileged users.
    pub blocks_free: u64,
    /// Number of free blocks available to an unprivileged user.
    pub blocks_available: u64,
}

/// Converts raw filesystem block counters into telemetry values.
///
/// The used percentage follows `df`: used space is divided by used plus space
/// available to an unprivileged user. A filesystem with a zero denominator is
/// reported as zero percent used.
///
/// # Errors
///
/// Returns an error when multiplication or addition would overflow `u64`, or
/// when the operating system reports more free bytes than total bytes.
pub fn calculate_disk_metrics(
    mount: impl Into<String>,
    values: StatvfsValues,
) -> Result<DiskMetrics, ParseError> {
    let total_bytes = bytes(values.blocks, values.fragment_size, "total size")?;
    let free_bytes = bytes(values.blocks_free, values.fragment_size, "free space")?;
    let available_bytes = bytes(
        values.blocks_available,
        values.fragment_size,
        "available space",
    )?;
    let used_bytes = total_bytes.checked_sub(free_bytes).ok_or_else(|| {
        ParseError::new(STATVFS, "free space is greater than the filesystem size")
    })?;
    let denominator = used_bytes
        .checked_add(available_bytes)
        .ok_or_else(|| ParseError::new(STATVFS, "used plus available space overflows u64"))?;
    let used_percent = if denominator == 0 {
        0.0
    } else {
        100.0 * used_bytes as f64 / denominator as f64
    };

    Ok(DiskMetrics {
        mount: mount.into(),
        total_bytes: Some(total_bytes),
        available_bytes: Some(available_bytes),
        used_bytes: Some(used_bytes),
        used_percent: Some(used_percent),
    })
}

fn bytes(blocks: u64, fragment_size: u64, name: &'static str) -> Result<u64, ParseError> {
    blocks
        .checked_mul(fragment_size)
        .ok_or_else(|| ParseError::new(STATVFS, format!("{name} overflows bytes as u64")))
}

#[cfg(test)]
mod tests {
    use super::{calculate_disk_metrics, StatvfsValues};

    #[test]
    fn calculates_sizes_and_df_compatible_percentage() {
        let metrics = calculate_disk_metrics(
            "/",
            StatvfsValues {
                fragment_size: 4_096,
                blocks: 1_000,
                blocks_free: 400,
                blocks_available: 300,
            },
        )
        .expect("valid counters should be calculated");

        assert_eq!(metrics.mount, "/");
        assert_eq!(metrics.total_bytes, Some(4_096_000));
        assert_eq!(metrics.available_bytes, Some(1_228_800));
        assert_eq!(metrics.used_bytes, Some(2_457_600));
        assert_eq!(metrics.used_percent, Some(100.0 * 600.0 / 900.0));
    }

    #[test]
    fn handles_an_empty_filesystem_without_dividing_by_zero() {
        let metrics = calculate_disk_metrics(
            "/empty",
            StatvfsValues {
                fragment_size: 4_096,
                blocks: 0,
                blocks_free: 0,
                blocks_available: 0,
            },
        )
        .expect("zero counters should remain representable");

        assert_eq!(metrics.used_percent, Some(0.0));
    }

    #[test]
    fn rejects_inconsistent_and_overflowing_counters() {
        let free_exceeds_total = StatvfsValues {
            fragment_size: 1,
            blocks: 10,
            blocks_free: 11,
            blocks_available: 0,
        };
        assert!(calculate_disk_metrics("/", free_exceeds_total).is_err());

        let multiplication_overflow = StatvfsValues {
            fragment_size: 2,
            blocks: u64::MAX,
            blocks_free: 0,
            blocks_available: 0,
        };
        assert!(calculate_disk_metrics("/", multiplication_overflow).is_err());

        let addition_overflow = StatvfsValues {
            fragment_size: 1,
            blocks: u64::MAX,
            blocks_free: 0,
            blocks_available: 1,
        };
        assert!(calculate_disk_metrics("/", addition_overflow).is_err());
    }
}
