//! Synchronous, dependency-injected Raspberry Pi health collection.
//!
//! A collection cycle reads independent Linux sources separately so one
//! unavailable measurement never discards the remaining snapshot. The only
//! subprocesses are direct `vcgencmd` invocations with a strict deadline.

use std::{
    fs, io,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;

use crate::{
    config::CollectorConfig,
    metrics::{
        cpu::{parse_frequency_khz, parse_loadavg, parse_proc_stat, usage_percent, CpuTimes},
        disk::{calculate_disk_metrics, StatvfsValues},
        memory::parse_meminfo,
        metadata::{parse_cpuinfo, parse_device_tree_model, parse_hostname, parse_os_release},
        power::{parse_throttled, ThrottledStatus},
        temperature::{parse_millidegrees_celsius, parse_vcgencmd_temperature},
        uptime::parse_uptime_seconds,
    },
    model::{
        CollectorError, CpuMetrics, DiskMetrics, MemoryMetrics, ObservationTime, PowerMetrics,
        SwapMetrics, TelemetryReadings, TelemetryState,
    },
};

const PROC_STAT: &str = "/proc/stat";
const PROC_LOADAVG: &str = "/proc/loadavg";
const PROC_MEMINFO: &str = "/proc/meminfo";
const PROC_UPTIME: &str = "/proc/uptime";
const PROC_CPUINFO: &str = "/proc/cpuinfo";
const DEVICE_TREE_MODEL: &str = "/proc/device-tree/model";
const KERNEL_RELEASE: &str = "/proc/sys/kernel/osrelease";
const HOSTNAME: &str = "/etc/hostname";
const OS_RELEASE: &str = "/etc/os-release";
const THERMAL_ZONE: &str = "/sys/class/thermal/thermal_zone0/temp";
const FREQUENCY_PATHS: &[&str] = &[
    "/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq",
    "/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_cur_freq",
];
const STANDARD_VCGENCMD_PATHS: &[&str] = &["/usr/bin/vcgencmd", "/opt/vc/bin/vcgencmd"];
const CPU_WARMUP: Duration = Duration::from_secs(1);
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_COMMAND_OUTPUT_BYTES: usize = 4_096;

/// Static device information detected once when a collector is created.
///
/// Every field is optional because metadata must never prevent health
/// collection. Configured identity remains authoritative when this information
/// is later used for MQTT discovery.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceMetadata {
    /// Hostname reported by `/etc/hostname`.
    pub hostname: Option<String>,
    /// Hardware model reported by the device tree.
    pub model: Option<String>,
    /// Raspberry Pi board revision reported by `/proc/cpuinfo`.
    pub board_revision: Option<String>,
    /// Raspberry Pi board serial reported by `/proc/cpuinfo`.
    pub serial_number: Option<String>,
    /// Running kernel release.
    pub kernel_release: Option<String>,
    /// Human-friendly operating-system name and version.
    pub operating_system: Option<String>,
}

/// Failure to create a usable collector.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum CollectorInitError {
    /// Neither the configured firmware tool nor a standard installation path exists.
    #[error("vcgencmd was not found at the configured or standard installation paths")]
    FirmwareCommandUnavailable,
}

/// Collects complete telemetry snapshots from Linux and Raspberry Pi sources.
///
/// The type is intentionally synchronous. The daemon runs collection on a
/// blocking worker so filesystem and firmware access cannot stall the MQTT
/// event loop.
pub struct Collector {
    root_filesystem: PathBuf,
    vcgencmd_path: PathBuf,
    command_timeout: Duration,
    previous_cpu: Option<CpuTimes>,
    metadata: DeviceMetadata,
    source: Box<dyn SystemSource>,
    command_runner: Box<dyn CommandRunner>,
    clock: Box<dyn Clock>,
}

impl Collector {
    /// Creates a collector and resolves the firmware command path.
    ///
    /// The configured path is preferred. If it is unavailable, the common
    /// `/usr/bin/vcgencmd` and `/opt/vc/bin/vcgencmd` locations are checked in
    /// that order. Hardware metadata is read once during construction.
    ///
    /// # Errors
    ///
    /// Returns an error when `vcgencmd` is absent from every permitted path.
    pub fn new(config: &CollectorConfig) -> Result<Self, CollectorInitError> {
        Self::with_dependencies(
            config.root_filesystem().to_path_buf(),
            config.vcgencmd_path().to_path_buf(),
            config.command_timeout(),
            Box::new(RealSystemSource),
            Box::new(ProcessCommandRunner),
            Box::new(SystemClock::new()),
        )
    }

    /// Returns device metadata captured when this collector was created.
    #[must_use]
    pub fn metadata(&self) -> &DeviceMetadata {
        &self.metadata
    }

    /// Collects one telemetry state payload.
    ///
    /// Individual failures are represented by `null` measurements and safe
    /// entries in `health.collector_errors`. After the first CPU sample, the
    /// collector waits one second for a useful delta; later cycles use the
    /// preceding sample without another warmup delay.
    #[must_use]
    pub fn collect(&mut self) -> TelemetryState {
        let started = self.clock.monotonic_now();
        let observed_at = self.clock.now_utc();
        let mut errors = Vec::new();
        let mut readings =
            TelemetryReadings::unavailable(self.root_filesystem.to_string_lossy().into_owned());

        readings.cpu.usage_percent = self.collect_cpu_usage(&mut errors);
        self.collect_load_average(&mut readings.cpu, &mut errors);
        self.collect_temperature(&mut readings.cpu, &mut errors);
        self.collect_frequency(&mut readings.cpu, &mut errors);
        self.collect_memory(&mut readings, &mut errors);
        self.collect_uptime(&mut readings, &mut errors);
        self.collect_disk(&mut readings.disk, &mut errors);
        self.collect_power(&mut readings.power, &mut errors);

        let duration = self
            .clock
            .monotonic_now()
            .saturating_duration_since(started);
        let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        TelemetryState::new(observed_at, readings, errors, duration_ms)
    }

    fn with_dependencies(
        root_filesystem: PathBuf,
        configured_vcgencmd: PathBuf,
        command_timeout: Duration,
        source: Box<dyn SystemSource>,
        command_runner: Box<dyn CommandRunner>,
        clock: Box<dyn Clock>,
    ) -> Result<Self, CollectorInitError> {
        let vcgencmd_path = resolve_vcgencmd(source.as_ref(), &configured_vcgencmd)?;
        let metadata = detect_metadata(source.as_ref());
        Ok(Self {
            root_filesystem,
            vcgencmd_path,
            command_timeout,
            previous_cpu: None,
            metadata,
            source,
            command_runner,
            clock,
        })
    }

    fn collect_cpu_usage(&mut self, errors: &mut Vec<CollectorError>) -> Option<f64> {
        let previous = match self.previous_cpu {
            Some(previous) => previous,
            None => {
                let initial = self
                    .read_cpu_sample()
                    .map_err(|()| {
                        push_error(
                            errors,
                            "cpu.usage_percent",
                            "aggregate CPU counters are unavailable or malformed",
                        );
                    })
                    .ok()?;
                self.previous_cpu = Some(initial);
                self.clock.sleep(CPU_WARMUP);
                initial
            }
        };

        let current = match self.read_cpu_sample() {
            Ok(current) => current,
            Err(()) => {
                push_error(
                    errors,
                    "cpu.usage_percent",
                    "aggregate CPU counters are unavailable or malformed",
                );
                return None;
            }
        };
        self.previous_cpu = Some(current);
        match usage_percent(&previous, &current) {
            Ok(value) => value,
            Err(_) => {
                push_error(
                    errors,
                    "cpu.usage_percent",
                    "aggregate CPU counters are inconsistent",
                );
                None
            }
        }
    }

    fn read_cpu_sample(&self) -> Result<CpuTimes, ()> {
        self.source
            .read_text(Path::new(PROC_STAT))
            .map_err(|_| ())
            .and_then(|input| parse_proc_stat(&input).map_err(|_| ()))
    }

    fn collect_load_average(&self, cpu: &mut CpuMetrics, errors: &mut Vec<CollectorError>) {
        let result = self
            .source
            .read_text(Path::new(PROC_LOADAVG))
            .map_err(|_| ())
            .and_then(|input| parse_loadavg(&input).map_err(|_| ()));
        match result {
            Ok(load) => {
                cpu.load_1 = Some(load.one_minute);
                cpu.load_5 = Some(load.five_minutes);
                cpu.load_15 = Some(load.fifteen_minutes);
            }
            Err(()) => push_error(
                errors,
                "cpu.load_average",
                "load averages are unavailable or malformed",
            ),
        }
    }

    fn collect_temperature(&self, cpu: &mut CpuMetrics, errors: &mut Vec<CollectorError>) {
        match self.source.read_text(Path::new(THERMAL_ZONE)) {
            Ok(input) => match parse_millidegrees_celsius(&input) {
                Ok(value) => cpu.temperature_c = Some(value),
                Err(_) => push_error(
                    errors,
                    "cpu.temperature_c",
                    "the thermal-zone temperature is malformed",
                ),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match self.command_runner.run(
                    &self.vcgencmd_path,
                    &["measure_temp"],
                    self.command_timeout,
                ) {
                    Ok(output) => match parse_vcgencmd_temperature(&output) {
                        Ok(value) => cpu.temperature_c = Some(value),
                        Err(_) => push_error(
                            errors,
                            "cpu.temperature_c",
                            "the firmware temperature response is malformed",
                        ),
                    },
                    Err(_) => push_error(
                        errors,
                        "cpu.temperature_c",
                        "the firmware temperature command failed or timed out",
                    ),
                }
            }
            Err(_) => push_error(
                errors,
                "cpu.temperature_c",
                "the thermal-zone temperature cannot be read",
            ),
        }
    }

    fn collect_frequency(&self, cpu: &mut CpuMetrics, errors: &mut Vec<CollectorError>) {
        for path in FREQUENCY_PATHS {
            match self.source.read_text(Path::new(path)) {
                Ok(input) => {
                    match parse_frequency_khz(&input) {
                        Ok(value) => cpu.frequency_mhz = Some(value),
                        Err(_) => push_error(
                            errors,
                            "cpu.frequency_mhz",
                            "the CPU frequency value is malformed",
                        ),
                    }
                    return;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => {
                    push_error(
                        errors,
                        "cpu.frequency_mhz",
                        "the CPU frequency cannot be read",
                    );
                    return;
                }
            }
        }
    }

    fn collect_memory(&self, readings: &mut TelemetryReadings, errors: &mut Vec<CollectorError>) {
        let result = self
            .source
            .read_text(Path::new(PROC_MEMINFO))
            .map_err(|_| ())
            .and_then(|input| parse_meminfo(&input).map_err(|_| ()));
        match result {
            Ok(memory) => {
                readings.memory = MemoryMetrics {
                    total_bytes: Some(memory.total_bytes),
                    available_bytes: Some(memory.available_bytes),
                    used_bytes: Some(memory.used_bytes),
                    used_percent: Some(memory.used_percent),
                };
                readings.swap = SwapMetrics {
                    total_bytes: Some(memory.swap_total_bytes),
                    used_bytes: Some(memory.swap_used_bytes),
                    used_percent: Some(memory.swap_used_percent),
                };
            }
            Err(()) => push_error(
                errors,
                "memory",
                "memory and swap information is unavailable or malformed",
            ),
        }
    }

    fn collect_uptime(&self, readings: &mut TelemetryReadings, errors: &mut Vec<CollectorError>) {
        let result = self
            .source
            .read_text(Path::new(PROC_UPTIME))
            .map_err(|_| ())
            .and_then(|input| parse_uptime_seconds(&input).map_err(|_| ()));
        match result {
            Ok(value) => readings.uptime_seconds = Some(value),
            Err(()) => push_error(
                errors,
                "uptime_seconds",
                "system uptime is unavailable or malformed",
            ),
        }
    }

    fn collect_disk(&self, disk: &mut DiskMetrics, errors: &mut Vec<CollectorError>) {
        match self
            .source
            .statvfs(&self.root_filesystem)
            .and_then(|values| {
                calculate_disk_metrics(self.root_filesystem.to_string_lossy(), values)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid statvfs data"))
            }) {
            Ok(value) => *disk = value,
            Err(_) => push_error(
                errors,
                "disk",
                "filesystem statistics are unavailable or inconsistent",
            ),
        }
    }

    fn collect_power(&self, power: &mut PowerMetrics, errors: &mut Vec<CollectorError>) {
        let result = self
            .command_runner
            .run(
                &self.vcgencmd_path,
                &["get_throttled"],
                self.command_timeout,
            )
            .map_err(|_| ())
            .and_then(|output| parse_throttled(&output).map_err(|_| ()));
        match result {
            Ok(status) => *power = power_metrics(status),
            Err(()) => push_error(
                errors,
                "power",
                "the firmware throttling command failed, timed out, or returned malformed data",
            ),
        }
    }
}

fn power_metrics(status: ThrottledStatus) -> PowerMetrics {
    PowerMetrics {
        throttled_raw: Some(status.raw_hex()),
        undervoltage_now: Some(status.undervoltage_now),
        arm_frequency_capped_now: Some(status.arm_frequency_capped_now),
        throttled_now: Some(status.throttled_now),
        soft_temperature_limit_now: Some(status.soft_temperature_limit_now),
        undervoltage_since_boot: Some(status.undervoltage_since_boot),
        arm_frequency_capped_since_boot: Some(status.arm_frequency_capped_since_boot),
        throttled_since_boot: Some(status.throttled_since_boot),
        soft_temperature_limit_since_boot: Some(status.soft_temperature_limit_since_boot),
    }
}

fn push_error(errors: &mut Vec<CollectorError>, metric: &'static str, message: &'static str) {
    errors.push(CollectorError::new(metric, message));
}

fn resolve_vcgencmd(
    source: &dyn SystemSource,
    configured: &Path,
) -> Result<PathBuf, CollectorInitError> {
    if source.is_file(configured) {
        return Ok(configured.to_path_buf());
    }
    STANDARD_VCGENCMD_PATHS
        .iter()
        .map(Path::new)
        .find(|candidate| *candidate != configured && source.is_file(candidate))
        .map(Path::to_path_buf)
        .ok_or(CollectorInitError::FirmwareCommandUnavailable)
}

fn detect_metadata(source: &dyn SystemSource) -> DeviceMetadata {
    let hostname = source
        .read_text(Path::new(HOSTNAME))
        .ok()
        .and_then(|value| parse_hostname(&value));
    let model = source
        .read_bytes(Path::new(DEVICE_TREE_MODEL))
        .ok()
        .and_then(|value| parse_device_tree_model(&value));
    let cpuinfo = source
        .read_text(Path::new(PROC_CPUINFO))
        .ok()
        .map(|value| parse_cpuinfo(&value))
        .unwrap_or_default();
    let kernel_release = source
        .read_text(Path::new(KERNEL_RELEASE))
        .ok()
        .and_then(|value| first_non_empty_line(&value));
    let operating_system = source
        .read_text(Path::new(OS_RELEASE))
        .ok()
        .and_then(|value| parse_os_release(&value).ok())
        .and_then(|release| {
            release
                .pretty_name
                .or_else(|| match (release.name, release.version) {
                    (Some(name), Some(version)) if !version.is_empty() => {
                        Some(format!("{name} {version}"))
                    }
                    (Some(name), _) => Some(name),
                    _ => None,
                })
        });

    DeviceMetadata {
        hostname,
        model,
        board_revision: cpuinfo.revision,
        serial_number: cpuinfo.serial,
        kernel_release,
        operating_system,
    }
}

fn first_non_empty_line(input: &str) -> Option<String> {
    input
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

trait SystemSource: Send + Sync {
    fn read_text(&self, path: &Path) -> io::Result<String>;
    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn is_file(&self, path: &Path) -> bool;
    fn statvfs(&self, path: &Path) -> io::Result<StatvfsValues>;
}

struct RealSystemSource;

impl SystemSource for RealSystemSource {
    fn read_text(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
    }

    #[cfg(target_os = "linux")]
    fn statvfs(&self, path: &Path) -> io::Result<StatvfsValues> {
        let values = rustix::fs::statvfs(path)
            .map_err(|error| io::Error::from_raw_os_error(error.raw_os_error()))?;
        Ok(StatvfsValues {
            fragment_size: values.f_frsize,
            blocks: values.f_blocks,
            blocks_free: values.f_bfree,
            blocks_available: values.f_bavail,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn statvfs(&self, _path: &Path) -> io::Result<StatvfsValues> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "statvfs is only available on Linux",
        ))
    }
}

trait CommandRunner: Send + Sync {
    fn run(
        &self,
        program: &Path,
        arguments: &[&str],
        timeout: Duration,
    ) -> Result<String, CommandFailure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandFailure {
    Spawn,
    Wait,
    TimedOut,
    NonZeroExit,
    OutputRead,
    OutputTooLarge,
    OutputEncoding,
}

struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(
        &self,
        program: &Path,
        arguments: &[&str],
        timeout: Duration,
    ) -> Result<String, CommandFailure> {
        let mut child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| CommandFailure::Spawn)?;
        let stdout = child.stdout.take().ok_or(CommandFailure::OutputRead)?;
        let output_reader = thread::spawn(move || read_limited_output(stdout));
        let started = Instant::now();

        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() < timeout => {
                    thread::sleep(
                        COMMAND_POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())),
                    );
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = output_reader.join();
                    return Err(CommandFailure::TimedOut);
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = output_reader.join();
                    return Err(CommandFailure::Wait);
                }
            }
        };
        let output = output_reader
            .join()
            .map_err(|_| CommandFailure::OutputRead)??;
        if !status.success() {
            return Err(CommandFailure::NonZeroExit);
        }
        String::from_utf8(output).map_err(|_| CommandFailure::OutputEncoding)
    }
}

fn read_limited_output(mut stdout: impl Read) -> Result<Vec<u8>, CommandFailure> {
    let mut output = Vec::with_capacity(MAX_COMMAND_OUTPUT_BYTES.min(256));
    let mut buffer = [0_u8; 512];
    let mut too_large = false;
    loop {
        let count = stdout
            .read(&mut buffer)
            .map_err(|_| CommandFailure::OutputRead)?;
        if count == 0 {
            break;
        }
        let remaining = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(remaining)]);
        too_large |= count > remaining;
    }
    if too_large {
        Err(CommandFailure::OutputTooLarge)
    } else {
        Ok(output)
    }
}

trait Clock: Send + Sync {
    fn now_utc(&self) -> ObservationTime;
    fn monotonic_now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

struct SystemClock;

impl SystemClock {
    fn new() -> Self {
        Self
    }
}

impl Clock for SystemClock {
    fn now_utc(&self) -> ObservationTime {
        ObservationTime::now_utc()
    }

    fn monotonic_now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet, VecDeque},
        io::Cursor,
        sync::{Arc, Mutex},
    };

    use crate::model::HealthStatus;

    use super::*;

    const FIRST_CPU: &str = include_str!("../tests/fixtures/proc_stat_1.txt");
    const SECOND_CPU: &str = include_str!("../tests/fixtures/proc_stat_2.txt");
    const MEMINFO: &str = include_str!("../tests/fixtures/proc_meminfo.txt");

    #[derive(Clone)]
    struct FakeSystemSource {
        inner: Arc<FakeSystemInner>,
    }

    struct FakeSystemInner {
        files: Mutex<BTreeMap<PathBuf, VecDeque<FakeRead>>>,
        regular_files: Mutex<BTreeSet<PathBuf>>,
        statvfs: Mutex<Option<Result<StatvfsValues, io::ErrorKind>>>,
        reads: Mutex<BTreeMap<PathBuf, usize>>,
    }

    enum FakeRead {
        Data(Vec<u8>),
        Error(io::ErrorKind),
    }

    impl FakeSystemSource {
        fn new() -> Self {
            Self {
                inner: Arc::new(FakeSystemInner {
                    files: Mutex::new(BTreeMap::new()),
                    regular_files: Mutex::new(BTreeSet::new()),
                    statvfs: Mutex::new(None),
                    reads: Mutex::new(BTreeMap::new()),
                }),
            }
        }

        fn add_regular_file(&self, path: &str) {
            self.inner
                .regular_files
                .lock()
                .expect("regular-file lock should be usable")
                .insert(PathBuf::from(path));
        }

        fn add_text(&self, path: &str, value: &str) {
            self.add_bytes(path, value.as_bytes());
        }

        fn add_bytes(&self, path: &str, value: &[u8]) {
            self.inner
                .files
                .lock()
                .expect("file lock should be usable")
                .entry(PathBuf::from(path))
                .or_default()
                .push_back(FakeRead::Data(value.to_vec()));
        }

        fn add_error(&self, path: &str, kind: io::ErrorKind) {
            self.inner
                .files
                .lock()
                .expect("file lock should be usable")
                .entry(PathBuf::from(path))
                .or_default()
                .push_back(FakeRead::Error(kind));
        }

        fn set_statvfs(&self, value: Result<StatvfsValues, io::ErrorKind>) {
            *self
                .inner
                .statvfs
                .lock()
                .expect("statvfs lock should be usable") = Some(value);
        }

        fn read_count(&self, path: &str) -> usize {
            self.inner
                .reads
                .lock()
                .expect("read-count lock should be usable")
                .get(Path::new(path))
                .copied()
                .unwrap_or(0)
        }

        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            *self
                .inner
                .reads
                .lock()
                .expect("read-count lock should be usable")
                .entry(path.to_path_buf())
                .or_default() += 1;
            match self
                .inner
                .files
                .lock()
                .expect("file lock should be usable")
                .get_mut(path)
                .and_then(VecDeque::pop_front)
            {
                Some(FakeRead::Data(value)) => Ok(value),
                Some(FakeRead::Error(kind)) => Err(io::Error::new(kind, "private test detail")),
                None => Err(io::Error::new(io::ErrorKind::NotFound, "missing fake")),
            }
        }
    }

    impl SystemSource for FakeSystemSource {
        fn read_text(&self, path: &Path) -> io::Result<String> {
            String::from_utf8(self.read(path)?)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 fake input"))
        }

        fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.read(path)
        }

        fn is_file(&self, path: &Path) -> bool {
            self.inner
                .regular_files
                .lock()
                .expect("regular-file lock should be usable")
                .contains(path)
        }

        fn statvfs(&self, _path: &Path) -> io::Result<StatvfsValues> {
            self.inner
                .statvfs
                .lock()
                .expect("statvfs lock should be usable")
                .take()
                .unwrap_or(Err(io::ErrorKind::NotFound))
                .map_err(|kind| io::Error::new(kind, "private statvfs detail"))
        }
    }

    #[derive(Clone)]
    struct FakeCommandRunner {
        inner: Arc<FakeCommandInner>,
    }

    struct FakeCommandInner {
        responses: Mutex<BTreeMap<String, VecDeque<Result<String, CommandFailure>>>>,
        calls: Mutex<Vec<(PathBuf, Vec<String>, Duration)>>,
    }

    impl FakeCommandRunner {
        fn new() -> Self {
            Self {
                inner: Arc::new(FakeCommandInner {
                    responses: Mutex::new(BTreeMap::new()),
                    calls: Mutex::new(Vec::new()),
                }),
            }
        }

        fn respond(&self, argument: &str, response: Result<&str, CommandFailure>) {
            self.inner
                .responses
                .lock()
                .expect("response lock should be usable")
                .entry(argument.to_owned())
                .or_default()
                .push_back(response.map(str::to_owned));
        }

        fn calls(&self) -> Vec<(PathBuf, Vec<String>, Duration)> {
            self.inner
                .calls
                .lock()
                .expect("call lock should be usable")
                .clone()
        }
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(
            &self,
            program: &Path,
            arguments: &[&str],
            timeout: Duration,
        ) -> Result<String, CommandFailure> {
            self.inner
                .calls
                .lock()
                .expect("call lock should be usable")
                .push((
                    program.to_path_buf(),
                    arguments.iter().map(|value| (*value).to_owned()).collect(),
                    timeout,
                ));
            let key = arguments.first().copied().unwrap_or_default();
            self.inner
                .responses
                .lock()
                .expect("response lock should be usable")
                .get_mut(key)
                .and_then(VecDeque::pop_front)
                .unwrap_or(Err(CommandFailure::Spawn))
        }
    }

    #[derive(Clone)]
    struct FakeClock {
        inner: Arc<FakeClockInner>,
    }

    struct FakeClockInner {
        base: Instant,
        elapsed: Mutex<Duration>,
        sleeps: Mutex<Vec<Duration>>,
        observed_at: ObservationTime,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                inner: Arc::new(FakeClockInner {
                    base: Instant::now(),
                    elapsed: Mutex::new(Duration::ZERO),
                    sleeps: Mutex::new(Vec::new()),
                    observed_at: ObservationTime::parse("2026-08-16T12:34:56Z")
                        .expect("test timestamp should parse"),
                }),
            }
        }

        fn sleeps(&self) -> Vec<Duration> {
            self.inner
                .sleeps
                .lock()
                .expect("sleep lock should be usable")
                .clone()
        }
    }

    impl Clock for FakeClock {
        fn now_utc(&self) -> ObservationTime {
            self.inner.observed_at
        }

        fn monotonic_now(&self) -> Instant {
            self.inner.base
                + *self
                    .inner
                    .elapsed
                    .lock()
                    .expect("elapsed lock should be usable")
        }

        fn sleep(&self, duration: Duration) {
            self.inner
                .sleeps
                .lock()
                .expect("sleep lock should be usable")
                .push(duration);
            *self
                .inner
                .elapsed
                .lock()
                .expect("elapsed lock should be usable") += duration;
        }
    }

    fn populated_source() -> FakeSystemSource {
        let source = FakeSystemSource::new();
        source.add_regular_file("/custom/vcgencmd");
        source.add_text(PROC_STAT, FIRST_CPU);
        source.add_text(PROC_STAT, SECOND_CPU);
        source.add_text(PROC_LOADAVG, "0.25 0.50 0.75 1/10 5\n");
        source.add_text(THERMAL_ZONE, "49875\n");
        source.add_text(FREQUENCY_PATHS[0], "900000\n");
        source.add_text(PROC_MEMINFO, MEMINFO);
        source.add_text(PROC_UPTIME, "12345.67 10.0\n");
        source.set_statvfs(Ok(StatvfsValues {
            fragment_size: 4_096,
            blocks: 1_000,
            blocks_free: 400,
            blocks_available: 300,
        }));
        source
    }

    fn build_collector(
        source: FakeSystemSource,
        runner: FakeCommandRunner,
        clock: FakeClock,
    ) -> Collector {
        Collector::with_dependencies(
            PathBuf::from("/"),
            PathBuf::from("/custom/vcgencmd"),
            Duration::from_secs(2),
            Box::new(source),
            Box::new(runner),
            Box::new(clock),
        )
        .expect("the fake firmware command should resolve")
    }

    #[test]
    fn collects_a_complete_snapshot_and_warms_up_cpu_once() {
        let source = populated_source();
        let runner = FakeCommandRunner::new();
        runner.respond("get_throttled", Ok("throttled=0x50005\n"));
        let clock = FakeClock::new();
        let mut collector = build_collector(source, runner.clone(), clock.clone());

        let state = collector.collect();

        assert_eq!(state.observed_at.to_string(), "2026-08-16T12:34:56Z");
        assert_eq!(state.cpu.usage_percent, Some(50.0));
        assert_eq!(state.cpu.load_1, Some(0.25));
        assert_eq!(state.cpu.temperature_c, Some(49.875));
        assert_eq!(state.cpu.frequency_mhz, Some(900.0));
        assert_eq!(state.memory.used_percent, Some(60.0));
        assert_eq!(state.swap.used_percent, Some(60.0));
        assert_eq!(state.disk.used_bytes, Some(2_457_600));
        assert_eq!(state.uptime_seconds, Some(12_345));
        assert_eq!(state.power.throttled_raw.as_deref(), Some("0x50005"));
        assert_eq!(state.health.status, HealthStatus::Critical);
        assert!(state.health.collector_errors.is_empty());
        assert_eq!(state.service.collection_duration_ms, 1_000);
        assert_eq!(clock.sleeps(), vec![CPU_WARMUP]);
        assert_eq!(runner.calls().len(), 1);
        assert_eq!(runner.calls()[0].1, vec!["get_throttled"]);
    }

    #[test]
    fn temperature_falls_back_only_when_sysfs_is_not_found() {
        let source = populated_source();
        source.add_error(THERMAL_ZONE, io::ErrorKind::NotFound);
        let runner = FakeCommandRunner::new();
        runner.respond("measure_temp", Ok("temp=52.5'C\n"));
        runner.respond("get_throttled", Ok("throttled=0x0\n"));
        let mut collector = build_collector(source, runner.clone(), FakeClock::new());

        let state = collector.collect();

        assert_eq!(state.cpu.temperature_c, Some(49.875));
        assert_eq!(runner.calls().len(), 1);

        let source = populated_source();
        source.add_error(THERMAL_ZONE, io::ErrorKind::NotFound);
        let mut files = source
            .inner
            .files
            .lock()
            .expect("file lock should be usable");
        files
            .get_mut(Path::new(THERMAL_ZONE))
            .expect("temperature queue should exist")
            .pop_front();
        drop(files);
        let runner = FakeCommandRunner::new();
        runner.respond("measure_temp", Ok("temp=52.5'C\n"));
        runner.respond("get_throttled", Ok("throttled=0x0\n"));
        let mut collector = build_collector(source, runner.clone(), FakeClock::new());
        let state = collector.collect();

        assert_eq!(state.cpu.temperature_c, Some(52.5));
        assert_eq!(runner.calls().len(), 2);
        assert_eq!(runner.calls()[0].1, vec!["measure_temp"]);
        assert_eq!(runner.calls()[1].1, vec!["get_throttled"]);
    }

    #[test]
    fn malformed_sysfs_temperature_does_not_invoke_the_fallback() {
        let source = populated_source();
        let mut files = source
            .inner
            .files
            .lock()
            .expect("file lock should be usable");
        *files
            .get_mut(Path::new(THERMAL_ZONE))
            .expect("temperature queue should exist") =
            VecDeque::from([FakeRead::Data(b"malformed\n".to_vec())]);
        drop(files);
        let runner = FakeCommandRunner::new();
        runner.respond("get_throttled", Ok("throttled=0x0\n"));
        let mut collector = build_collector(source, runner.clone(), FakeClock::new());

        let state = collector.collect();

        assert_eq!(state.cpu.temperature_c, None);
        assert_eq!(runner.calls().len(), 1);
        assert_eq!(runner.calls()[0].1, vec!["get_throttled"]);
    }

    #[test]
    fn partial_failures_produce_nulls_and_fixed_safe_errors() {
        let source = FakeSystemSource::new();
        source.add_regular_file("/custom/vcgencmd");
        source.add_error(THERMAL_ZONE, io::ErrorKind::PermissionDenied);
        source.set_statvfs(Err(io::ErrorKind::PermissionDenied));
        let runner = FakeCommandRunner::new();
        runner.respond("get_throttled", Err(CommandFailure::TimedOut));
        let mut collector = build_collector(source, runner, FakeClock::new());

        let state = collector.collect();

        assert_eq!(state.cpu.usage_percent, None);
        assert_eq!(state.cpu.frequency_mhz, None);
        assert_eq!(state.memory.total_bytes, None);
        assert_eq!(state.disk.total_bytes, None);
        assert_eq!(state.power.undervoltage_now, None);
        assert_eq!(state.health.status, HealthStatus::Degraded);
        assert_eq!(state.health.collector_errors.len(), 7);
        let serialized = serde_json::to_string(&state).expect("partial state should serialize");
        assert!(!serialized.contains("private test detail"));
        assert!(!serialized.contains("private statvfs detail"));
    }

    #[test]
    fn later_cycles_reuse_the_cpu_baseline_without_sleeping_again() {
        let source = populated_source();
        source.add_text(PROC_STAT, "cpu 250 0 150 900 50 20 20 10\n");
        let runner = FakeCommandRunner::new();
        runner.respond("get_throttled", Ok("throttled=0x0\n"));
        runner.respond("get_throttled", Ok("throttled=0x0\n"));
        let clock = FakeClock::new();
        let mut collector = build_collector(source.clone(), runner, clock.clone());

        let _ = collector.collect();
        let _ = collector.collect();

        assert_eq!(source.read_count(PROC_STAT), 3);
        assert_eq!(clock.sleeps(), vec![CPU_WARMUP]);
    }

    #[test]
    fn detects_optional_metadata_exactly_once() {
        let source = populated_source();
        source.add_text(HOSTNAME, "kitchen-node\n");
        source.add_bytes(DEVICE_TREE_MODEL, b"Example Board\0");
        source.add_text(PROC_CPUINFO, "Revision : a02082\nSerial : 00000001\n");
        source.add_text(KERNEL_RELEASE, "6.12.0-example\n");
        source.add_text(OS_RELEASE, "PRETTY_NAME=\"Example Linux 1\"\n");
        let runner = FakeCommandRunner::new();
        runner.respond("get_throttled", Ok("throttled=0x0\n"));
        let mut collector = build_collector(source.clone(), runner, FakeClock::new());

        assert_eq!(
            collector.metadata().hostname.as_deref(),
            Some("kitchen-node")
        );
        assert_eq!(collector.metadata().model.as_deref(), Some("Example Board"));
        assert_eq!(
            collector.metadata().board_revision.as_deref(),
            Some("a02082")
        );
        assert_eq!(
            collector.metadata().serial_number.as_deref(),
            Some("00000001")
        );
        assert_eq!(
            collector.metadata().kernel_release.as_deref(),
            Some("6.12.0-example")
        );
        assert_eq!(
            collector.metadata().operating_system.as_deref(),
            Some("Example Linux 1")
        );
        let _ = collector.collect();
        for path in [
            HOSTNAME,
            DEVICE_TREE_MODEL,
            PROC_CPUINFO,
            KERNEL_RELEASE,
            OS_RELEASE,
        ] {
            assert_eq!(source.read_count(path), 1, "{path}");
        }
    }

    #[test]
    fn resolves_configured_then_standard_firmware_paths() {
        let source = FakeSystemSource::new();
        source.add_regular_file("/custom/vcgencmd");
        assert_eq!(
            resolve_vcgencmd(&source, Path::new("/custom/vcgencmd")),
            Ok(PathBuf::from("/custom/vcgencmd"))
        );

        let source = FakeSystemSource::new();
        source.add_regular_file("/usr/bin/vcgencmd");
        assert_eq!(
            resolve_vcgencmd(&source, Path::new("/missing/vcgencmd")),
            Ok(PathBuf::from("/usr/bin/vcgencmd"))
        );

        let source = FakeSystemSource::new();
        assert!(matches!(
            resolve_vcgencmd(&source, Path::new("/missing/vcgencmd")),
            Err(CollectorInitError::FirmwareCommandUnavailable)
        ));
    }

    #[test]
    fn output_reader_enforces_the_four_kibibyte_limit() {
        assert_eq!(
            read_limited_output(Cursor::new(vec![b'x'; MAX_COMMAND_OUTPUT_BYTES]))
                .expect("the exact limit should be accepted")
                .len(),
            MAX_COMMAND_OUTPUT_BYTES
        );
        assert_eq!(
            read_limited_output(Cursor::new(vec![b'x'; MAX_COMMAND_OUTPUT_BYTES + 1])),
            Err(CommandFailure::OutputTooLarge)
        );
    }
}
