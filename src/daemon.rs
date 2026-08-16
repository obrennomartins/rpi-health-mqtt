//! Long-lived monitoring service orchestration.
//!
//! The daemon keeps asynchronous MQTT I/O on a single runtime thread while a
//! permanent blocking worker owns the synchronous collector. Collection
//! requests and serialized states use [`tokio::sync::watch`] channels: both are
//! bounded, and bursts replace stale work instead of building an unbounded
//! history.

use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use thiserror::Error;
use tokio::{
    runtime::{Builder, Runtime},
    sync::watch,
    task::{JoinError, JoinHandle},
    time::{Instant, Interval, MissedTickBehavior},
};
use tracing::{error, info, warn};

use crate::{
    collector::{Collector, CollectorInitError},
    config::{Config, MqttCredentials},
    discovery::{build_discovery_message, DiscoveryMessage, DiscoverySettings},
    mqtt::{
        request_collection, MqttError, MqttSettings, MqttSupervisor, StateMessage,
        GRACEFUL_SHUTDOWN_TIMEOUT,
    },
};

const MAX_BLOCKING_THREADS: usize = 2;
const COLLECTOR_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const SERVICE_MANAGER_STOP_BUDGET: Duration = Duration::from_secs(10);
const BUSY_WARNING_INTERVAL: Duration = Duration::from_secs(60);
const _: () = assert!(
    GRACEFUL_SHUTDOWN_TIMEOUT.as_secs()
        + COLLECTOR_DRAIN_TIMEOUT.as_secs()
        + RUNTIME_SHUTDOWN_TIMEOUT.as_secs()
        < SERVICE_MANAGER_STOP_BUDGET.as_secs()
);

/// An unrecoverable monitoring runtime failure.
///
/// Broker and network outages are deliberately not represented here because
/// the MQTT supervisor reconnects after those conditions indefinitely.
#[derive(Debug, Error)]
pub enum DaemonError {
    /// The current-thread asynchronous runtime could not be created.
    #[error("the asynchronous runtime could not be initialized")]
    RuntimeInitialization {
        /// Underlying runtime construction failure.
        #[source]
        source: io::Error,
    },
    /// A collector could not be created from the validated configuration.
    #[error("the telemetry collector could not be initialized")]
    CollectorInitialization {
        /// Sanitized collector initialization failure.
        #[source]
        source: CollectorInitError,
    },
    /// The deterministic discovery document could not be serialized.
    #[error("the discovery document could not be serialized")]
    DiscoverySerialization {
        /// JSON serialization failure.
        #[source]
        source: serde_json::Error,
    },
    /// A telemetry state could not be serialized.
    #[error("a telemetry state could not be serialized")]
    StateSerialization {
        /// JSON serialization failure.
        #[source]
        source: serde_json::Error,
    },
    /// Operating-system signal handling could not be installed or completed.
    #[error("the shutdown signal handler failed")]
    SignalHandling {
        /// Underlying signal registration failure.
        #[source]
        source: io::Error,
    },
    /// The MQTT supervisor encountered an internal control failure.
    #[error("the MQTT supervisor encountered an unrecoverable control failure")]
    Mqtt {
        /// Sanitized MQTT supervisor failure.
        #[source]
        source: MqttError,
    },
    /// A runtime task ended before shutdown was requested.
    #[error("the {task} task stopped unexpectedly")]
    TaskStopped {
        /// Stable, non-sensitive component name.
        task: &'static str,
    },
    /// A runtime task panicked or was cancelled.
    #[error("the {task} task failed")]
    TaskFailed {
        /// Stable, non-sensitive component name.
        task: &'static str,
    },
}

/// Installs compact, non-colored structured logging on standard error.
///
/// Calling this function after another global subscriber has been installed is
/// harmless; the existing subscriber is preserved.
pub fn initialize_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let subscriber = tracing_subscriber::fmt()
        .compact()
        .with_target(false)
        .with_ansi(false)
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// Runs the monitoring service until `SIGTERM` or `SIGINT` is received.
///
/// Collection uses a permanent blocking worker. The MQTT supervisor retries
/// recoverable broker and transport failures without terminating this method.
/// A normal operating-system shutdown signal publishes retained `offline`,
/// waits for its QoS 1 acknowledgement for at most three seconds, disconnects,
/// and returns success.
///
/// # Errors
///
/// Returns an error only when startup, signal handling, serialization, task
/// supervision, or bounded MQTT control fails irrecoverably.
pub fn run(config: Config, credentials: MqttCredentials) -> Result<(), DaemonError> {
    let collector = Collector::new(config.collector())
        .map_err(|source| DaemonError::CollectorInitialization { source })?;
    let discovery = discovery_message(&config, &collector)?;
    let mqtt_settings = MqttSettings::new(config.mqtt(), config.device().id());
    let interval = config.collector().interval();
    let runtime = build_runtime()?;

    let result = runtime.block_on(run_service(
        collector,
        discovery,
        mqtt_settings,
        credentials,
        interval,
    ));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    result
}

fn discovery_message(
    config: &Config,
    collector: &Collector,
) -> Result<DiscoveryMessage, DaemonError> {
    let metadata = collector.metadata();
    let mut settings = DiscoverySettings::new(
        config.device().id(),
        config.device().name(),
        config.mqtt().base_topic(),
        config.mqtt().discovery_prefix(),
        config.collector().interval(),
    );
    if let Some(model) = &metadata.model {
        settings = settings.with_model(model.clone());
    }
    if let Some(serial_number) = &metadata.serial_number {
        settings = settings.with_serial_number(serial_number.clone());
    }
    if let Some(board_revision) = &metadata.board_revision {
        settings = settings.with_hardware_version(board_revision.clone());
    }
    build_discovery_message(&settings)
        .map_err(|source| DaemonError::DiscoverySerialization { source })
}

fn build_runtime() -> Result<Runtime, DaemonError> {
    Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(MAX_BLOCKING_THREADS)
        .thread_name("rpi-health-worker")
        .build()
        .map_err(|source| DaemonError::RuntimeInitialization { source })
}

async fn run_service(
    collector: Collector,
    discovery: DiscoveryMessage,
    mqtt_settings: MqttSettings,
    credentials: MqttCredentials,
    interval: Duration,
) -> Result<(), DaemonError> {
    let (request_sender, request_receiver) = watch::channel(0_u64);
    let (state_sender, state_receiver) = watch::channel(None);
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let collection_busy = Arc::new(AtomicBool::new(false));

    let runtime_handle = tokio::runtime::Handle::current();
    let worker_busy = Arc::clone(&collection_busy);
    let mut collector_task = tokio::task::spawn_blocking(move || {
        collection_worker(
            collector,
            request_receiver,
            state_sender,
            &runtime_handle,
            &worker_busy,
        )
    });
    let mut scheduler_task = tokio::spawn(collection_scheduler(
        interval,
        request_sender.clone(),
        shutdown_receiver.clone(),
        collection_busy,
    ));
    let (mqtt, _) = MqttSupervisor::new(
        mqtt_settings,
        credentials,
        &discovery,
        state_receiver,
        request_sender.clone(),
        shutdown_receiver,
    );
    let mut mqtt_task = tokio::spawn(mqtt.run());
    drop(request_sender);

    info!("monitoring service started");
    let completion = tokio::select! {
        biased;
        signal = shutdown_signal() => Completion::Signal(signal),
        result = &mut mqtt_task => Completion::Mqtt(result),
        result = &mut scheduler_task => Completion::Scheduler(result),
        result = &mut collector_task => Completion::Collector(result),
    };

    shutdown_sender.send_replace(true);
    let result = match completion {
        Completion::Signal(signal) => {
            if signal.is_ok() {
                info!("shutdown signal received");
            } else {
                error!("shutdown signal handler failed");
            }
            let primary = signal.map_err(|source| DaemonError::SignalHandling { source });
            let (mqtt, scheduler, collector) = tokio::join!(
                finish_mqtt(mqtt_task),
                finish_task(scheduler_task, "collection scheduler"),
                drain_collector(collector_task),
            );
            primary.and(mqtt).and(scheduler).and(collector)
        }
        Completion::Mqtt(completed) => {
            error!(component = "mqtt supervisor", "runtime component stopped");
            let primary = unexpected_mqtt(completed);
            let (scheduler, collector) = tokio::join!(
                finish_task(scheduler_task, "collection scheduler"),
                drain_collector(collector_task),
            );
            primary.and(scheduler).and(collector)
        }
        Completion::Scheduler(completed) => {
            error!(
                component = "collection scheduler",
                "runtime component stopped"
            );
            let primary = unexpected_task(completed, "collection scheduler");
            let (mqtt, collector) =
                tokio::join!(finish_mqtt(mqtt_task), drain_collector(collector_task));
            primary.and(mqtt).and(collector)
        }
        Completion::Collector(completed) => {
            error!(component = "collector worker", "runtime component stopped");
            let primary = unexpected_collector(completed);
            let (mqtt, scheduler) = tokio::join!(
                finish_mqtt(mqtt_task),
                finish_task(scheduler_task, "collection scheduler"),
            );
            primary.and(mqtt).and(scheduler)
        }
    };

    if result.is_ok() {
        info!("monitoring service stopped");
    }
    result
}

enum Completion {
    Signal(io::Result<()>),
    Mqtt(Result<Result<(), MqttError>, JoinError>),
    Scheduler(Result<(), JoinError>),
    Collector(Result<Result<(), DaemonError>, JoinError>),
}

async fn finish_mqtt(task: JoinHandle<Result<(), MqttError>>) -> Result<(), DaemonError> {
    match task.await {
        Ok(result) => result.map_err(|source| DaemonError::Mqtt { source }),
        Err(_) => Err(DaemonError::TaskFailed {
            task: "MQTT supervisor",
        }),
    }
}

async fn finish_collector(task: JoinHandle<Result<(), DaemonError>>) -> Result<(), DaemonError> {
    match task.await {
        Ok(result) => result,
        Err(_) => Err(DaemonError::TaskFailed {
            task: "collector worker",
        }),
    }
}

async fn drain_collector(task: JoinHandle<Result<(), DaemonError>>) -> Result<(), DaemonError> {
    match finish_collector_with_timeout(task, COLLECTOR_DRAIN_TIMEOUT).await? {
        true => Ok(()),
        false => {
            info!("collector still running; its pending state was discarded");
            Ok(())
        }
    }
}

async fn finish_collector_with_timeout(
    task: JoinHandle<Result<(), DaemonError>>,
    timeout: Duration,
) -> Result<bool, DaemonError> {
    match tokio::time::timeout(timeout, finish_collector(task)).await {
        Ok(result) => result.map(|()| true),
        Err(_) => Ok(false),
    }
}

async fn finish_task(task: JoinHandle<()>, name: &'static str) -> Result<(), DaemonError> {
    task.await
        .map_err(|_| DaemonError::TaskFailed { task: name })
}

fn unexpected_mqtt(result: Result<Result<(), MqttError>, JoinError>) -> Result<(), DaemonError> {
    match result {
        Ok(Ok(())) => Err(DaemonError::TaskStopped {
            task: "MQTT supervisor",
        }),
        Ok(Err(source)) => Err(DaemonError::Mqtt { source }),
        Err(_) => Err(DaemonError::TaskFailed {
            task: "MQTT supervisor",
        }),
    }
}

fn unexpected_collector(
    result: Result<Result<(), DaemonError>, JoinError>,
) -> Result<(), DaemonError> {
    match result {
        Ok(Ok(())) => Err(DaemonError::TaskStopped {
            task: "collector worker",
        }),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(DaemonError::TaskFailed {
            task: "collector worker",
        }),
    }
}

fn unexpected_task(result: Result<(), JoinError>, name: &'static str) -> Result<(), DaemonError> {
    match result {
        Ok(()) => Err(DaemonError::TaskStopped { task: name }),
        Err(_) => Err(DaemonError::TaskFailed { task: name }),
    }
}

trait SnapshotCollector: Send + 'static {
    fn collect_payload(&mut self) -> Result<Vec<u8>, DaemonError>;
}

impl SnapshotCollector for Collector {
    fn collect_payload(&mut self) -> Result<Vec<u8>, DaemonError> {
        serde_json::to_vec(&self.collect())
            .map_err(|source| DaemonError::StateSerialization { source })
    }
}

fn collection_worker<C: SnapshotCollector>(
    mut collector: C,
    mut requests: watch::Receiver<u64>,
    states: watch::Sender<Option<StateMessage>>,
    runtime: &tokio::runtime::Handle,
    busy: &AtomicBool,
) -> Result<(), DaemonError> {
    while runtime.block_on(requests.changed()).is_ok() {
        let request_id = *requests.borrow_and_update();
        if request_id == 0 {
            continue;
        }
        busy.store(true, Ordering::Release);
        let result = collector.collect_payload();
        if let Ok(payload) = &result {
            states.send_replace(Some(StateMessage::new(
                request_id,
                Arc::<[u8]>::from(payload.clone()),
            )));
        }
        busy.store(false, Ordering::Release);
        result?;
    }
    Ok(())
}

async fn collection_scheduler(
    period: Duration,
    requests: watch::Sender<u64>,
    mut shutdown: watch::Receiver<bool>,
    busy: Arc<AtomicBool>,
) {
    if *shutdown.borrow() {
        return;
    }
    request_collection(&requests);
    let mut interval = collection_interval(period);
    let mut last_busy_warning = None;
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            _ = interval.tick() => {
                if busy.load(Ordering::Acquire) {
                    let now = Instant::now();
                    if last_busy_warning.is_none_or(|last: Instant| {
                        now.duration_since(last) >= BUSY_WARNING_INTERVAL
                    }) {
                        warn!("collection still running; periodic tick skipped");
                        last_busy_warning = Some(now);
                    }
                } else {
                    request_collection(&requests);
                }
            }
        }
    }
}

fn collection_interval(period: Duration) -> Interval {
    let mut interval = tokio::time::interval_at(Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval
}

#[cfg(unix)]
async fn shutdown_signal() -> io::Result<()> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    tokio::select! {
        received = terminate.recv() => received.ok_or_else(signal_stream_closed),
        received = interrupt.recv() => received.ok_or_else(signal_stream_closed),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> io::Result<()> {
    tokio::signal::ctrl_c().await
}

#[cfg(unix)]
fn signal_stream_closed() -> io::Error {
    io::Error::other("the operating-system signal stream closed")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{mpsc, Arc, Mutex},
        thread,
        time::Duration,
    };

    use tokio::runtime::RuntimeFlavor;

    use super::*;

    struct FakeCollector {
        payloads: VecDeque<Vec<u8>>,
        calls: Arc<Mutex<usize>>,
    }

    impl FakeCollector {
        fn new(payloads: impl IntoIterator<Item = &'static [u8]>) -> (Self, Arc<Mutex<usize>>) {
            let calls = Arc::new(Mutex::new(0));
            (
                Self {
                    payloads: payloads.into_iter().map(<[u8]>::to_vec).collect(),
                    calls: Arc::clone(&calls),
                },
                calls,
            )
        }
    }

    impl SnapshotCollector for FakeCollector {
        fn collect_payload(&mut self) -> Result<Vec<u8>, DaemonError> {
            *self.calls.lock().expect("call counter should be usable") += 1;
            Ok(self.payloads.pop_front().unwrap_or_default())
        }
    }

    #[test]
    fn runtime_uses_one_async_thread() {
        let runtime = build_runtime().expect("runtime should build");
        assert_eq!(
            runtime.handle().runtime_flavor(),
            RuntimeFlavor::CurrentThread
        );
    }

    #[test]
    fn configured_shutdown_bounds_fit_the_service_manager_budget() {
        let conservative_bound =
            GRACEFUL_SHUTDOWN_TIMEOUT + COLLECTOR_DRAIN_TIMEOUT + RUNTIME_SHUTDOWN_TIMEOUT;
        assert!(conservative_bound < SERVICE_MANAGER_STOP_BUDGET);
    }

    #[test]
    fn pending_collection_requests_are_coalesced() {
        let (request_sender, request_receiver) = watch::channel(0_u64);
        let first = request_collection(&request_sender);
        let second = request_collection(&request_sender);
        let third = request_collection(&request_sender);
        assert_eq!([first, second, third], [1, 2, 3]);

        let (state_sender, state_receiver) = watch::channel(None);
        let (collector, calls) = FakeCollector::new([b"latest".as_slice()]);
        let runtime = build_runtime().expect("runtime should build");
        let busy = AtomicBool::new(false);
        drop(request_sender);
        collection_worker(
            collector,
            request_receiver,
            state_sender,
            runtime.handle(),
            &busy,
        )
        .expect("worker should stop cleanly");

        let state = state_receiver
            .borrow()
            .clone()
            .expect("latest state should be available");
        assert_eq!(state.request_id, 3);
        assert_eq!(state.payload.as_ref(), b"latest");
        assert_eq!(*calls.lock().expect("call counter should be usable"), 1);
    }

    struct PausingCollector {
        first_started: Option<mpsc::Sender<()>>,
        release_first: mpsc::Receiver<()>,
        sequence: u8,
    }

    impl SnapshotCollector for PausingCollector {
        fn collect_payload(&mut self) -> Result<Vec<u8>, DaemonError> {
            self.sequence = self.sequence.saturating_add(1);
            if let Some(started) = self.first_started.take() {
                started.send(()).expect("test receiver should remain open");
                self.release_first
                    .recv_timeout(Duration::from_secs(1))
                    .expect("test should release the first collection");
            }
            Ok(vec![self.sequence])
        }
    }

    #[test]
    fn a_new_request_supersedes_an_inflight_state() {
        let (request_sender, request_receiver) = watch::channel(0_u64);
        let (state_sender, mut state_receiver) = watch::channel(None);
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let collector = PausingCollector {
            first_started: Some(started_sender),
            release_first: release_receiver,
            sequence: 0,
        };
        let runtime = build_runtime().expect("runtime should build");
        let runtime_handle = runtime.handle().clone();
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = Arc::clone(&busy);
        let worker = thread::spawn(move || {
            collection_worker(
                collector,
                request_receiver,
                state_sender,
                &runtime_handle,
                &worker_busy,
            )
        });

        assert_eq!(request_collection(&request_sender), 1);
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("first collection should start");
        assert_eq!(request_collection(&request_sender), 2);
        release_sender
            .send(())
            .expect("worker should still be running");

        loop {
            runtime
                .block_on(state_receiver.changed())
                .expect("worker should publish a state");
            let request_id = state_receiver
                .borrow_and_update()
                .as_ref()
                .expect("state should be present")
                .request_id;
            if request_id == 2 {
                break;
            }
        }
        drop(request_sender);
        worker
            .join()
            .expect("worker should not panic")
            .expect("worker should stop cleanly");
        assert_eq!(
            state_receiver
                .borrow()
                .as_ref()
                .expect("latest state should remain")
                .request_id,
            2
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scheduler_requests_immediately_and_stops_on_shutdown() {
        let (request_sender, mut request_receiver) = watch::channel(0_u64);
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let scheduler = tokio::spawn(collection_scheduler(
            Duration::from_secs(3_600),
            request_sender,
            shutdown_receiver,
            Arc::new(AtomicBool::new(false)),
        ));

        request_receiver
            .changed()
            .await
            .expect("initial request should be emitted");
        assert_eq!(*request_receiver.borrow_and_update(), 1);
        shutdown_sender.send_replace(true);
        tokio::time::timeout(Duration::from_millis(100), scheduler)
            .await
            .expect("scheduler should stop promptly")
            .expect("scheduler should not panic");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scheduler_skips_missed_ticks() {
        let interval = collection_interval(Duration::from_secs(60));
        assert_eq!(interval.missed_tick_behavior(), MissedTickBehavior::Skip);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scheduler_drops_periodic_ticks_while_collection_is_busy() {
        let (request_sender, mut request_receiver) = watch::channel(0_u64);
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let busy = Arc::new(AtomicBool::new(false));
        let scheduler = tokio::spawn(collection_scheduler(
            Duration::from_millis(40),
            request_sender,
            shutdown_receiver,
            Arc::clone(&busy),
        ));
        request_receiver
            .changed()
            .await
            .expect("initial request should be emitted");
        assert_eq!(*request_receiver.borrow_and_update(), 1);

        busy.store(true, Ordering::Release);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*request_receiver.borrow_and_update(), 1);

        busy.store(false, Ordering::Release);
        tokio::time::timeout(Duration::from_millis(80), request_receiver.changed())
            .await
            .expect("the next normal tick should be emitted")
            .expect("request channel should remain open");
        assert_eq!(*request_receiver.borrow_and_update(), 2);
        shutdown_sender.send_replace(true);
        scheduler.await.expect("scheduler should not panic");
    }

    struct SlowCollector;

    impl SnapshotCollector for SlowCollector {
        fn collect_payload(&mut self) -> Result<Vec<u8>, DaemonError> {
            thread::sleep(Duration::from_millis(50));
            Ok(b"state".to_vec())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_collection_does_not_stall_the_async_runtime() {
        let (request_sender, request_receiver) = watch::channel(0_u64);
        let (state_sender, _) = watch::channel(None);
        let runtime_handle = tokio::runtime::Handle::current();
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = Arc::clone(&busy);
        let worker = tokio::task::spawn_blocking(move || {
            collection_worker(
                SlowCollector,
                request_receiver,
                state_sender,
                &runtime_handle,
                &worker_busy,
            )
        });
        request_collection(&request_sender);

        tokio::time::timeout(
            Duration::from_millis(20),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .expect("the async timer should not be blocked by collection");
        drop(request_sender);
        worker
            .await
            .expect("worker should not panic")
            .expect("worker should stop cleanly");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_stops_waiting_for_a_blocked_collector() {
        let worker = tokio::task::spawn_blocking(|| {
            thread::sleep(Duration::from_millis(100));
            Ok(())
        });

        let finished = finish_collector_with_timeout(worker, Duration::from_millis(5))
            .await
            .expect("a timeout is a clean discarded-result outcome");
        assert!(!finished);
    }
}
