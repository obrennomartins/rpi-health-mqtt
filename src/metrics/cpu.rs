//! CPU counter, load-average, and frequency parsers.

use super::ParseError;

const PROC_STAT: &str = "/proc/stat";
const PROC_LOADAVG: &str = "/proc/loadavg";
const CPU_FREQUENCY: &str = "CPU frequency";

/// The aggregate CPU counters used to calculate processor utilization.
///
/// Linux reports these values in jiffies. Guest counters are deliberately not
/// included because the kernel already includes them in `user` and `nice`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuTimes {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    io_wait: u64,
    irq: u64,
    soft_irq: u64,
    steal: u64,
}

impl CpuTimes {
    /// Returns all idle jiffies, including time waiting for I/O.
    pub fn idle_jiffies(&self) -> Result<u64, ParseError> {
        self.idle
            .checked_add(self.io_wait)
            .ok_or_else(|| ParseError::new(PROC_STAT, "the aggregate idle counter overflowed u64"))
    }

    /// Returns all tracked jiffies in this sample.
    pub fn total_jiffies(&self) -> Result<u64, ParseError> {
        checked_sum(
            &[
                self.user,
                self.nice,
                self.system,
                self.idle,
                self.io_wait,
                self.irq,
                self.soft_irq,
                self.steal,
            ],
            "the aggregate CPU counter overflowed u64",
        )
    }
}

/// The system load averages over one, five, and fifteen minutes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoadAverage {
    /// Load average over the previous minute.
    pub one_minute: f64,
    /// Load average over the previous five minutes.
    pub five_minutes: f64,
    /// Load average over the previous fifteen minutes.
    pub fifteen_minutes: f64,
}

/// Parses the aggregate `cpu` line from `/proc/stat`.
///
/// At least the eight counters through `steal` must be present. Additional
/// kernel counters are ignored.
pub fn parse_proc_stat(input: &str) -> Result<CpuTimes, ParseError> {
    let line = input
        .lines()
        .find(|line| line.split_whitespace().next() == Some("cpu"))
        .ok_or_else(|| ParseError::new(PROC_STAT, "the aggregate cpu line is missing"))?;
    let mut fields = line.split_whitespace();
    let _label = fields.next();
    let names = [
        "user", "nice", "system", "idle", "iowait", "irq", "softirq", "steal",
    ];
    let mut counters = [0_u64; 8];

    for (index, name) in names.iter().enumerate() {
        let raw = fields
            .next()
            .ok_or_else(|| ParseError::new(PROC_STAT, format!("the {name} counter is missing")))?;
        counters[index] = raw
            .parse::<u64>()
            .map_err(|_| ParseError::new(PROC_STAT, format!("the {name} counter is not a u64")))?;
    }

    Ok(CpuTimes {
        user: counters[0],
        nice: counters[1],
        system: counters[2],
        idle: counters[3],
        io_wait: counters[4],
        irq: counters[5],
        soft_irq: counters[6],
        steal: counters[7],
    })
}

/// Calculates total CPU usage between two chronologically ordered samples.
///
/// `Ok(None)` indicates that none of the tracked counters advanced. A counter
/// rollback or arithmetic overflow is returned as an error so it can never be
/// mistaken for a valid utilization spike.
pub fn usage_percent(previous: &CpuTimes, current: &CpuTimes) -> Result<Option<f64>, ParseError> {
    let names = [
        "user", "nice", "system", "idle", "iowait", "irq", "softirq", "steal",
    ];
    let old = [
        previous.user,
        previous.nice,
        previous.system,
        previous.idle,
        previous.io_wait,
        previous.irq,
        previous.soft_irq,
        previous.steal,
    ];
    let new = [
        current.user,
        current.nice,
        current.system,
        current.idle,
        current.io_wait,
        current.irq,
        current.soft_irq,
        current.steal,
    ];
    let mut delta = [0_u64; 8];

    for index in 0..delta.len() {
        delta[index] = new[index].checked_sub(old[index]).ok_or_else(|| {
            ParseError::new(
                PROC_STAT,
                format!("the {} counter moved backwards", names[index]),
            )
        })?;
    }

    let idle_delta = delta[3]
        .checked_add(delta[4])
        .ok_or_else(|| ParseError::new(PROC_STAT, "the aggregate idle delta overflowed u64"))?;
    let total_delta = checked_sum(&delta, "the aggregate CPU delta overflowed u64")?;
    if total_delta == 0 {
        return Ok(None);
    }

    let busy_delta = total_delta
        .checked_sub(idle_delta)
        .ok_or_else(|| ParseError::new(PROC_STAT, "the idle delta exceeded the total CPU delta"))?;
    Ok(Some(100.0 * busy_delta as f64 / total_delta as f64))
}

/// Parses the first three values from `/proc/loadavg`.
pub fn parse_loadavg(input: &str) -> Result<LoadAverage, ParseError> {
    let mut fields = input.split_whitespace();
    let one_minute = parse_non_negative_float(fields.next(), "one-minute load")?;
    let five_minutes = parse_non_negative_float(fields.next(), "five-minute load")?;
    let fifteen_minutes = parse_non_negative_float(fields.next(), "fifteen-minute load")?;

    Ok(LoadAverage {
        one_minute,
        five_minutes,
        fifteen_minutes,
    })
}

/// Parses a sysfs CPU frequency in kilohertz and returns megahertz.
pub fn parse_frequency_khz(input: &str) -> Result<f64, ParseError> {
    let mut fields = input.split_whitespace();
    let raw = fields
        .next()
        .ok_or_else(|| ParseError::new(CPU_FREQUENCY, "the value is missing"))?;
    if fields.next().is_some() {
        return Err(ParseError::new(
            CPU_FREQUENCY,
            "the value contains unexpected fields",
        ));
    }

    let kilohertz = raw
        .parse::<u64>()
        .map_err(|_| ParseError::new(CPU_FREQUENCY, "the value is not an unsigned integer"))?;
    Ok(kilohertz as f64 / 1_000.0)
}

fn parse_non_negative_float(raw: Option<&str>, name: &str) -> Result<f64, ParseError> {
    let raw = raw.ok_or_else(|| ParseError::new(PROC_LOADAVG, format!("{name} is missing")))?;
    let value = raw
        .parse::<f64>()
        .map_err(|_| ParseError::new(PROC_LOADAVG, format!("{name} is not a number")))?;
    if !value.is_finite() || value.is_sign_negative() {
        return Err(ParseError::new(
            PROC_LOADAVG,
            format!("{name} must be finite and non-negative"),
        ));
    }
    Ok(value)
}

fn checked_sum(values: &[u64], overflow_detail: &'static str) -> Result<u64, ParseError> {
    values.iter().try_fold(0_u64, |total, value| {
        total
            .checked_add(*value)
            .ok_or_else(|| ParseError::new(PROC_STAT, overflow_detail))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        parse_frequency_khz, parse_loadavg, parse_proc_stat, usage_percent, CpuTimes, LoadAverage,
    };

    const FIRST_SAMPLE: &str = include_str!("../../tests/fixtures/proc_stat_1.txt");
    const SECOND_SAMPLE: &str = include_str!("../../tests/fixtures/proc_stat_2.txt");

    #[test]
    fn parses_aggregate_cpu_counters_from_fixture() {
        let sample = parse_proc_stat(FIRST_SAMPLE).expect("fixture should be valid");

        assert_eq!(sample.total_jiffies().expect("sum should fit"), 1_000);
        assert_eq!(sample.idle_jiffies().expect("sum should fit"), 850);
    }

    #[test]
    fn calculates_cpu_delta_without_counting_idle_time_as_busy() {
        let first = parse_proc_stat(FIRST_SAMPLE).expect("first fixture should be valid");
        let second = parse_proc_stat(SECOND_SAMPLE).expect("second fixture should be valid");

        assert_eq!(usage_percent(&first, &second), Ok(Some(50.0)));
    }

    #[test]
    fn returns_none_when_cpu_counters_do_not_advance() {
        let sample = parse_proc_stat(FIRST_SAMPLE).expect("fixture should be valid");

        assert_eq!(usage_percent(&sample, &sample), Ok(None));
    }

    #[test]
    fn rejects_a_counter_rollback() {
        let previous =
            parse_proc_stat("cpu 100 10 20 800 50 2 3 4").expect("previous sample should be valid");
        let current =
            parse_proc_stat("cpu 99 10 20 900 50 2 3 4").expect("current sample should be valid");

        let error = usage_percent(&previous, &current).expect_err("rollback must fail");
        assert!(error.detail().contains("user counter moved backwards"));
    }

    #[test]
    fn rejects_an_overflowing_cpu_delta() {
        let previous =
            parse_proc_stat("cpu 0 0 0 0 0 0 0 0").expect("previous sample should be valid");
        let current = parse_proc_stat(&format!("cpu {0} {0} {0} {0} {0} {0} {0} {0}", u64::MAX))
            .expect("individual counters should be valid");

        let error = usage_percent(&previous, &current).expect_err("overflow must fail");
        assert!(error.detail().contains("overflowed u64"));
    }

    #[test]
    fn rejects_missing_truncated_and_invalid_cpu_lines() {
        assert!(parse_proc_stat("intr 10\n").is_err());
        assert!(parse_proc_stat("cpu 1 2 3 4 5 6 7").is_err());
        assert!(parse_proc_stat("cpu 1 2 3 invalid 5 6 7 8").is_err());
    }

    #[test]
    fn ignores_per_core_lines_and_extra_aggregate_fields() {
        let input = "cpu0 9 9 9 9 9 9 9 9\ncpu 1 2 3 4 5 6 7 8 9 10\n";

        let sample = parse_proc_stat(input).expect("aggregate line should be found");
        assert_eq!(sample.total_jiffies().expect("sum should fit"), 36);
    }

    #[test]
    fn reports_overflow_when_cpu_totals_cannot_be_represented() {
        let sample = CpuTimes {
            user: u64::MAX,
            nice: 1,
            system: 0,
            idle: 0,
            io_wait: 0,
            irq: 0,
            soft_irq: 0,
            steal: 0,
        };

        assert!(sample.total_jiffies().is_err());
    }

    #[test]
    fn parses_load_average() {
        assert_eq!(
            parse_loadavg("0.25 1.50 2.75 2/100 42\n"),
            Ok(LoadAverage {
                one_minute: 0.25,
                five_minutes: 1.5,
                fifteen_minutes: 2.75,
            })
        );
    }

    #[test]
    fn rejects_invalid_load_average() {
        for input in ["", "0.1 0.2", "NaN 0.2 0.3", "-0.1 0.2 0.3"] {
            assert!(parse_loadavg(input).is_err(), "input should fail: {input}");
        }
    }

    #[test]
    fn converts_cpu_frequency_to_megahertz() {
        assert_eq!(parse_frequency_khz("900000\n"), Ok(900.0));
        assert_eq!(parse_frequency_khz("1500500"), Ok(1_500.5));
    }

    #[test]
    fn rejects_invalid_cpu_frequency() {
        assert!(parse_frequency_khz("").is_err());
        assert!(parse_frequency_khz("-1").is_err());
        assert!(parse_frequency_khz("900000 kHz").is_err());
    }
}
