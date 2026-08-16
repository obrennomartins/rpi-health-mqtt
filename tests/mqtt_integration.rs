//! Authenticated real-broker validation for the MQTT publication contract.

#![cfg(target_os = "linux")]

use std::{
    fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::Arc,
    time::{Duration, Instant as StdInstant, SystemTime, UNIX_EPOCH},
};

use rpi_health_mqtt::{
    cli::{COLLECTION_ERROR, CONFIG_ERROR, DIAGNOSTIC_ERROR, SUCCESS},
    config::{Config, MqttCredentials},
    discovery::{build_discovery_message, DiscoverySettings},
    model::ObservationTime,
    mqtt::{MqttSettings, MqttStatus, MqttSupervisor, StateMessage},
};
use rumqttc::{
    AsyncClient, ConnectReturnCode, Event, EventLoop, Incoming, MqttOptions, QoS, SubscribeFilter,
};
use rustix::process::{kill_process, Pid, Signal};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
    task::JoinHandle,
};

const USERNAME: &str = "monitor-example";
const PASSWORD: &str = "test-only-password";
const BASE_TOPIC: &str = "example/monitor/example-pi";
const DISCOVERY_TOPIC: &str = "homeassistant/device/example-pi/config";
const DAEMON_DISCOVERY_PREFIX: &str = "integration/discovery";
const SERVICE_BINARY: &str = "/usr/local/bin/rpi-health-mqtt";
const DIAGNOSTIC_CANARY: &str = "diagnostic-secret-canary-never-render";
const PRINT_ONCE_CANARY: &str = "print-once-secret-canary-never-render";

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the Docker validation broker"]
async fn publication_retention_shutdown_and_authentication_contract() {
    let host = std::env::var("MQTT_TEST_HOST").expect("broker host must be configured");
    let port = std::env::var("MQTT_TEST_PORT")
        .expect("broker port must be configured")
        .parse::<u16>()
        .expect("broker port must be numeric");
    let password_path = write_password_fixture();
    let config = Config::parse(&configuration(&host, port, &password_path))
        .expect("integration configuration should be valid");
    let credentials = MqttCredentials::load(config.mqtt()).expect("credentials should load");
    let discovery_settings = DiscoverySettings::new(
        config.device().id(),
        config.device().name(),
        config.mqtt().base_topic(),
        config.mqtt().discovery_prefix(),
        config.collector().interval(),
    );
    let discovery =
        build_discovery_message(&discovery_settings).expect("discovery should serialize");

    let (observer, mut observer_events) = observer(&host, port, "observer-live", PASSWORD);
    connect(&mut observer_events).await;
    observer
        .subscribe_many([
            SubscribeFilter::new(format!("{BASE_TOPIC}/#"), QoS::AtLeastOnce),
            SubscribeFilter::new(DISCOVERY_TOPIC.to_owned(), QoS::AtLeastOnce),
        ])
        .await
        .expect("observer subscriptions should queue");

    let (state_tx, state_rx) = watch::channel(None);
    let (request_tx, mut request_rx) = watch::channel(0_u64);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let settings = MqttSettings::new(config.mqtt(), config.device().id());
    let (supervisor, mut status) = MqttSupervisor::new(
        settings,
        credentials,
        &discovery,
        state_rx,
        request_tx,
        shutdown_rx,
    );
    let state_responder = tokio::spawn(async move {
        while request_rx.changed().await.is_ok() {
            let request_id = *request_rx.borrow_and_update();
            state_tx.send_replace(Some(StateMessage::new(
                request_id,
                Arc::<[u8]>::from(
                    format!(r#"{{"schema_version":1,"request_id":{request_id}}}"#).into_bytes(),
                ),
            )));
        }
    });
    let supervisor_task = tokio::spawn(supervisor.run());

    let mut received = Vec::new();
    while received.len() < 3 {
        received.push(next_publish(&mut observer_events).await);
    }
    let discovery_publish = find_topic(&received, DISCOVERY_TOPIC);
    assert_eq!(discovery_publish.qos, QoS::AtLeastOnce);
    assert!(serde_json::from_slice::<serde_json::Value>(&discovery_publish.payload).is_ok());
    let online = find_topic(&received, &format!("{BASE_TOPIC}/availability"));
    assert_eq!(online.qos, QoS::AtLeastOnce);
    assert_eq!(online.payload.as_ref(), b"online");
    let state = find_topic(&received, &format!("{BASE_TOPIC}/state"));
    assert_eq!(state.qos, QoS::AtMostOnce);
    assert!(!state.retain);

    wait_for_status(&mut status, MqttStatus::Online).await;
    let (_late_client, mut late_events) = subscribed_observer(&host, port, "observer-late").await;
    let first = next_publish(&mut late_events).await;
    let second = next_publish(&mut late_events).await;
    let retained = [first, second];
    assert!(find_topic(&retained, DISCOVERY_TOPIC).retain);
    let retained_online = find_topic(&retained, &format!("{BASE_TOPIC}/availability"));
    assert!(retained_online.retain);
    assert_eq!(retained_online.payload.as_ref(), b"online");
    assert!(
        tokio::time::timeout(Duration::from_millis(400), next_publish(&mut late_events))
            .await
            .is_err(),
        "periodic state must not be retained"
    );

    shutdown_tx.send_replace(true);
    let offline = loop {
        let publish = next_publish(&mut observer_events).await;
        if publish.topic == format!("{BASE_TOPIC}/availability")
            && publish.payload.as_ref() == b"offline"
        {
            break publish;
        }
    };
    assert_eq!(offline.qos, QoS::AtLeastOnce);
    let result = tokio::time::timeout(Duration::from_secs(5), supervisor_task)
        .await
        .expect("supervisor should stop within its deadline")
        .expect("supervisor task should not panic");
    assert_eq!(result, Ok(()));

    let (_final_client, mut final_events) =
        subscribed_observer(&host, port, "observer-final").await;
    let retained_messages = [
        next_publish(&mut final_events).await,
        next_publish(&mut final_events).await,
    ];
    let retained_offline = find_topic(&retained_messages, &format!("{BASE_TOPIC}/availability"));
    assert!(retained_offline.retain);
    assert_eq!(retained_offline.payload.as_ref(), b"offline");

    assert_rejected_authentication_is_secret_safe(&host, port).await;
    state_responder.abort();
    fs::remove_file(password_path).expect("password fixture should be removed");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the Docker validation broker and service binary"]
async fn daemon_binary_recovers_and_obeys_process_lifecycle_contract() {
    let host = std::env::var("MQTT_TEST_HOST").expect("broker host must be configured");
    let port = std::env::var("MQTT_TEST_PORT")
        .expect("broker port must be configured")
        .parse::<u16>()
        .expect("broker port must be numeric");
    let proxy = FaultProxy::start(&host, port).await;

    let graceful_id = "daemon-graceful";
    let graceful_base = daemon_base_topic(graceful_id);
    let graceful_discovery = daemon_discovery_topic(graceful_id);
    let (_observer, mut observer_events) = subscribed_to_topics(
        &host,
        port,
        "daemon-graceful-observer",
        &graceful_base,
        &graceful_discovery,
    )
    .await;
    let graceful_fixture = DaemonFixture::new(graceful_id, &graceful_base, proxy.address());
    let mut graceful_process = spawn_daemon(&graceful_fixture.config_path);

    let initial =
        receive_bootstrap(&mut observer_events, &graceful_base, &graceful_discovery).await;
    assert_bootstrap_contract(&initial);
    let initial_observed_at = observed_at(&initial.state);
    assert_process_running(&mut graceful_process);
    assert_retained_lifecycle(
        &host,
        port,
        "daemon-initial-retained-observer",
        &graceful_base,
        &graceful_discovery,
        b"online",
    )
    .await;

    proxy.interrupt();
    let interrupted_offline = next_topic_publish(
        &mut observer_events,
        &format!("{graceful_base}/availability"),
        Duration::from_secs(8),
    )
    .await
    .expect("the broker should publish the last will after path interruption");
    assert_eq!(interrupted_offline.payload.as_ref(), b"offline");
    assert_eq!(interrupted_offline.qos, QoS::AtLeastOnce);
    assert_process_running(&mut graceful_process);

    let restored_at = ObservationTime::now_utc();
    proxy.restore();
    let recovered =
        receive_bootstrap(&mut observer_events, &graceful_base, &graceful_discovery).await;
    assert_bootstrap_contract(&recovered);
    let recovered_observed_at = observed_at(&recovered.state);
    assert!(recovered_observed_at > restored_at);
    assert!(recovered_observed_at > initial_observed_at);
    assert_process_running(&mut graceful_process);
    assert!(
        next_topic_publish(
            &mut observer_events,
            &format!("{graceful_base}/state"),
            Duration::from_secs(1),
        )
        .await
        .is_none(),
        "recovery must not replay historical state"
    );

    let graceful_stop_started = StdInstant::now();
    kill_process(Pid::from_child(&graceful_process), Signal::TERM)
        .expect("SIGTERM should be delivered to the service");
    let graceful_wait = tokio::task::spawn_blocking(move || graceful_process.wait_with_output());
    let graceful_offline = next_topic_publish(
        &mut observer_events,
        &format!("{graceful_base}/availability"),
        Duration::from_secs(8),
    )
    .await
    .expect("graceful shutdown should publish offline");
    assert_eq!(graceful_offline.payload.as_ref(), b"offline");
    assert_eq!(graceful_offline.qos, QoS::AtLeastOnce);
    let graceful_output = wait_for_output(graceful_wait, Duration::from_secs(9)).await;
    assert!(graceful_output.status.success());
    assert!(graceful_stop_started.elapsed() < Duration::from_secs(10));
    assert_daemon_output_is_safe(&graceful_output);
    assert_retained_lifecycle(
        &host,
        port,
        "daemon-graceful-final-observer",
        &graceful_base,
        &graceful_discovery,
        b"offline",
    )
    .await;
    drop(graceful_fixture);

    let abrupt_id = "daemon-abrupt";
    let abrupt_base = daemon_base_topic(abrupt_id);
    let abrupt_discovery = daemon_discovery_topic(abrupt_id);
    let (_abrupt_observer, mut abrupt_events) = subscribed_to_topics(
        &host,
        port,
        "daemon-abrupt-observer",
        &abrupt_base,
        &abrupt_discovery,
    )
    .await;
    let abrupt_fixture = DaemonFixture::new(abrupt_id, &abrupt_base, proxy.address());
    let mut abrupt_process = spawn_daemon(&abrupt_fixture.config_path);
    let abrupt_bootstrap =
        receive_bootstrap(&mut abrupt_events, &abrupt_base, &abrupt_discovery).await;
    assert_bootstrap_contract(&abrupt_bootstrap);
    abrupt_process
        .kill()
        .expect("the service should accept an abrupt kill");
    let abrupt_wait = tokio::task::spawn_blocking(move || abrupt_process.wait_with_output());
    let last_will = next_topic_publish(
        &mut abrupt_events,
        &format!("{abrupt_base}/availability"),
        Duration::from_secs(8),
    )
    .await
    .expect("an abrupt stop should trigger the last will");
    assert_eq!(last_will.payload.as_ref(), b"offline");
    assert_eq!(last_will.qos, QoS::AtLeastOnce);
    let abrupt_output = wait_for_output(abrupt_wait, Duration::from_secs(5)).await;
    assert!(!abrupt_output.status.success());
    assert_daemon_output_is_safe(&abrupt_output);
    assert_retained_lifecycle(
        &host,
        port,
        "daemon-abrupt-final-observer",
        &abrupt_base,
        &abrupt_discovery,
        b"offline",
    )
    .await;

    drop(abrupt_fixture);
    proxy.stop().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the Docker validation broker and service binary"]
async fn diagnostics_binary_is_bounded_secret_safe_and_never_publishes() {
    let host = std::env::var("MQTT_TEST_HOST").expect("broker host must be configured");
    let port = std::env::var("MQTT_TEST_PORT")
        .expect("broker port must be configured")
        .parse::<u16>()
        .expect("broker port must be numeric");

    let (observer_client, mut observer_events) =
        observer(&host, port, "diagnostics-observer", PASSWORD);
    connect(&mut observer_events).await;
    observer_client
        .subscribe("integration/diagnostics/#", QoS::AtLeastOnce)
        .await
        .expect("diagnostics observer subscription should queue");
    wait_for_subscription(&mut observer_events).await;
    let (publication_tx, mut publication_rx) = mpsc::unbounded_channel();
    let observer_task = tokio::spawn(async move {
        while let Ok(event) = observer_events.poll().await {
            if let Event::Incoming(Incoming::Publish(publish)) = event {
                let _ = publication_tx.send(publish);
            }
        }
    });

    let success = DiagnosticFixture::new("success", &host, port, PASSWORD, 0o600);
    let (output, elapsed) = run_diagnostic(&success.config_path, &[], Duration::from_secs(7)).await;
    assert_eq!(output.status.code(), Some(i32::from(SUCCESS)));
    assert!(elapsed < Duration::from_secs(5));
    let report = output_text(&output.stdout);
    assert!(report.contains("[PASS] credentials:"));
    assert!(report.contains("[PASS] runtime identity:"));
    assert!(report.contains("[WARNING] architecture: running on x86_64"));
    assert!(report.contains("collector:"));
    assert!(report.contains("[PASS] mqtt:"));
    assert!(!report.contains("[FAILURE]"));
    assert!(output.stderr.is_empty());
    assert_secret_absent(&output, DIAGNOSTIC_CANARY);

    let unused_listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("an unused local port should be reserved");
    let unavailable_port = unused_listener
        .local_addr()
        .expect("unused listener should have an address")
        .port();
    drop(unused_listener);

    let skipped = DiagnosticFixture::new("skipped", "127.0.0.1", unavailable_port, PASSWORD, 0o600);
    let (output, elapsed) = run_diagnostic(
        &skipped.config_path,
        &["--skip-mqtt"],
        Duration::from_secs(7),
    )
    .await;
    assert_eq!(output.status.code(), Some(i32::from(SUCCESS)));
    assert!(elapsed < Duration::from_secs(5));
    assert!(output_text(&output.stdout).contains("[SKIPPED] mqtt:"));
    assert!(output.stderr.is_empty());

    let rejected = DiagnosticFixture::new("rejected", &host, port, DIAGNOSTIC_CANARY, 0o600);
    let (output, elapsed) =
        run_diagnostic(&rejected.config_path, &[], Duration::from_secs(7)).await;
    assert_eq!(output.status.code(), Some(i32::from(DIAGNOSTIC_ERROR)));
    assert!(elapsed < Duration::from_secs(5));
    assert!(output_text(&output.stdout).contains("[FAILURE] mqtt:"));
    assert!(output.stderr.is_empty());
    assert_secret_absent(&output, DIAGNOSTIC_CANARY);

    let unavailable = DiagnosticFixture::new(
        "unavailable",
        "127.0.0.1",
        unavailable_port,
        DIAGNOSTIC_CANARY,
        0o600,
    );
    let (output, elapsed) =
        run_diagnostic(&unavailable.config_path, &[], Duration::from_secs(7)).await;
    assert_eq!(output.status.code(), Some(i32::from(DIAGNOSTIC_ERROR)));
    assert!(elapsed < Duration::from_secs(5));
    assert!(output_text(&output.stdout).contains("[FAILURE] mqtt:"));
    assert!(output.stderr.is_empty());
    assert_secret_absent(&output, DIAGNOSTIC_CANARY);

    let unsafe_credential = DiagnosticFixture::new("unsafe", &host, port, DIAGNOSTIC_CANARY, 0o644);
    let (output, elapsed) =
        run_diagnostic(&unsafe_credential.config_path, &[], Duration::from_secs(7)).await;
    assert_eq!(output.status.code(), Some(i32::from(DIAGNOSTIC_ERROR)));
    assert!(elapsed < Duration::from_secs(5));
    let report = output_text(&output.stdout);
    assert!(report.contains("[FAILURE] credentials:"));
    assert!(report.contains("[SKIPPED] mqtt:"));
    assert!(output.stderr.is_empty());
    assert_secret_absent(&output, DIAGNOSTIC_CANARY);

    let invalid = DiagnosticFixture::new("invalid", &host, port, DIAGNOSTIC_CANARY, 0o600);
    fs::write(&invalid.config_path, "invalid = [\n")
        .expect("invalid configuration fixture should be written");
    let (output, elapsed) = run_diagnostic(&invalid.config_path, &[], Duration::from_secs(3)).await;
    assert_eq!(output.status.code(), Some(i32::from(CONFIG_ERROR)));
    assert!(elapsed < Duration::from_secs(2));
    assert!(output.stdout.is_empty());
    assert!(output_text(&output.stderr).starts_with("Configuration error:"));
    assert_secret_absent(&output, DIAGNOSTIC_CANARY);

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        publication_rx.try_recv().is_err(),
        "the diagnostics command must not publish MQTT messages"
    );
    observer_task.abort();
    let _ = observer_task.await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the Docker validation broker and service binary"]
async fn print_once_binary_is_json_only_secret_safe_and_never_publishes() {
    use std::os::unix::fs::PermissionsExt;

    let host = std::env::var("MQTT_TEST_HOST").expect("broker host must be configured");
    let port = std::env::var("MQTT_TEST_PORT")
        .expect("broker port must be configured")
        .parse::<u16>()
        .expect("broker port must be numeric");

    let (observer_client, mut observer_events) =
        observer(&host, port, "print-once-observer", PASSWORD);
    connect(&mut observer_events).await;
    observer_client
        .subscribe("integration/diagnostics/#", QoS::AtLeastOnce)
        .await
        .expect("print-once observer subscription should queue");
    wait_for_subscription(&mut observer_events).await;
    let (publication_tx, mut publication_rx) = mpsc::unbounded_channel();
    let observer_task = tokio::spawn(async move {
        while let Ok(event) = observer_events.poll().await {
            if let Event::Incoming(Incoming::Publish(publish)) = event {
                let _ = publication_tx.send(publish);
            }
        }
    });

    let success =
        DiagnosticFixture::new("print-once-success", &host, port, PRINT_ONCE_CANARY, 0o000);
    let success_password = success.directory.join("mqtt-password");
    assert_eq!(
        fs::metadata(&success_password)
            .expect("print-once credential metadata should be readable")
            .permissions()
            .mode()
            & 0o777,
        0
    );
    assert_eq!(
        fs::read(&success_password)
            .expect_err("print-once credential must be unreadable")
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    let (output, elapsed) = run_print_once(&success.config_path, Duration::from_secs(7)).await;
    assert_eq!(output.status.code(), Some(i32::from(SUCCESS)));
    assert!(elapsed < Duration::from_secs(5));
    let document: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("print-once stdout should contain exactly one JSON document");
    assert!(document.is_object());
    assert!(output.stderr.is_empty());
    assert_secret_absent(&output, PRINT_ONCE_CANARY);

    let missing =
        DiagnosticFixture::new("print-once-missing", &host, port, PRINT_ONCE_CANARY, 0o000);
    fs::remove_file(missing.directory.join("vcgencmd"))
        .expect("fake firmware command should be removed");
    let (output, elapsed) = run_print_once(&missing.config_path, Duration::from_secs(3)).await;
    assert_eq!(output.status.code(), Some(i32::from(COLLECTION_ERROR)));
    assert!(elapsed < Duration::from_secs(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output_text(&output.stderr),
        "Collection error: vcgencmd was not found at the configured or standard installation paths\n"
    );
    assert_secret_absent(&output, PRINT_ONCE_CANARY);

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        publication_rx.try_recv().is_err(),
        "the print-once command must not publish MQTT messages"
    );
    observer_task.abort();
    let _ = observer_task.await;
}

struct DiagnosticFixture {
    directory: PathBuf,
    config_path: PathBuf,
}

impl DiagnosticFixture {
    fn new(case: &str, host: &str, port: u16, password: &str, password_mode: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "rpi-health-mqtt-diagnostics-integration-{}-{case}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("diagnostics fixture directory should be created");

        let password_path = directory.join("mqtt-password");
        fs::write(&password_path, format!("{password}\n"))
            .expect("diagnostics password fixture should be written");
        fs::set_permissions(&password_path, fs::Permissions::from_mode(password_mode))
            .expect("diagnostics password permissions should be configured");

        let vcgencmd_path = directory.join("vcgencmd");
        fs::write(
            &vcgencmd_path,
            br#"#!/bin/sh
case "$1" in
  get_throttled)
    printf '%s\n' 'throttled=0x0'
    ;;
  measure_temp)
    printf '%s\n' "temp=42.0'C"
    ;;
  *)
    exit 2
    ;;
esac
"#,
        )
        .expect("fake firmware command should be written");
        fs::set_permissions(&vcgencmd_path, fs::Permissions::from_mode(0o700))
            .expect("fake firmware command should be executable");

        let config_path = directory.join("config.toml");
        let config = format!(
            r#"
[device]
id = "diagnostics-{case}"
name = "Diagnostics Raspberry Pi"

[mqtt]
host = "{host}"
port = {port}
client_id = "rpi-health-mqtt-diagnostics-{case}"
username = "{USERNAME}"
password_file = "{}"
base_topic = "integration/diagnostics/{case}"
discovery_prefix = "integration/diagnostics/discovery"
keep_alive_seconds = 5

[collector]
interval_seconds = 30
root_filesystem = "/"
vcgencmd_path = "{}"
command_timeout_seconds = 2
"#,
            password_path.display(),
            vcgencmd_path.display(),
        );
        fs::write(&config_path, config).expect("diagnostics configuration should be written");

        Self {
            directory,
            config_path,
        }
    }
}

impl Drop for DiagnosticFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

async fn run_diagnostic(
    config_path: &Path,
    check_arguments: &[&str],
    timeout: Duration,
) -> (Output, Duration) {
    let binary =
        std::env::var("RPI_HEALTH_MQTT_TEST_BINARY").unwrap_or_else(|_| SERVICE_BINARY.to_owned());
    let mut process = Command::new(binary)
        .arg("--config")
        .arg(config_path)
        .arg("check")
        .args(check_arguments)
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("diagnostics process should start");
    let started = StdInstant::now();

    loop {
        if process
            .try_wait()
            .expect("diagnostics process status should be readable")
            .is_some()
        {
            let output = process
                .wait_with_output()
                .expect("diagnostics output should be collected");
            return (output, started.elapsed());
        }
        if started.elapsed() >= timeout {
            let _ = process.kill();
            let _ = process.wait();
            panic!("diagnostics process exceeded its test deadline");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn run_print_once(config_path: &Path, timeout: Duration) -> (Output, Duration) {
    let binary =
        std::env::var("RPI_HEALTH_MQTT_TEST_BINARY").unwrap_or_else(|_| SERVICE_BINARY.to_owned());
    let mut process = Command::new(binary)
        .arg("--config")
        .arg(config_path)
        .arg("print-once")
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("print-once process should start");
    let started = StdInstant::now();

    loop {
        if process
            .try_wait()
            .expect("print-once process status should be readable")
            .is_some()
        {
            let output = process
                .wait_with_output()
                .expect("print-once output should be collected");
            return (output, started.elapsed());
        }
        if started.elapsed() >= timeout {
            let _ = process.kill();
            let _ = process.wait();
            panic!("print-once process exceeded its test deadline");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("service output should be UTF-8")
}

fn assert_secret_absent(output: &Output, secret: &str) {
    assert!(
        !output
            .stdout
            .windows(secret.len())
            .any(|value| value == secret.as_bytes()),
        "service stdout must not expose the credential canary"
    );
    assert!(
        !output
            .stderr
            .windows(secret.len())
            .any(|value| value == secret.as_bytes()),
        "service stderr must not expose the credential canary"
    );
}

struct BootstrapPublications {
    discovery: rumqttc::Publish,
    online: rumqttc::Publish,
    state: rumqttc::Publish,
}

async fn receive_bootstrap(
    events: &mut EventLoop,
    base_topic: &str,
    discovery_topic: &str,
) -> BootstrapPublications {
    let availability_topic = format!("{base_topic}/availability");
    let state_topic = format!("{base_topic}/state");
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut discovery = None;
        let mut online = None;
        let mut state = None;
        while discovery.is_none() || online.is_none() || state.is_none() {
            let publish = loop {
                if let Event::Incoming(Incoming::Publish(publish)) = events
                    .poll()
                    .await
                    .expect("observer event loop should remain connected")
                {
                    break publish;
                }
            };
            if publish.topic == discovery_topic {
                discovery = Some(publish);
            } else if publish.topic == availability_topic && publish.payload.as_ref() == b"online" {
                online = Some(publish);
            } else if publish.topic == state_topic {
                state = Some(publish);
            }
        }
        BootstrapPublications {
            discovery: discovery.expect("discovery should be present"),
            online: online.expect("online should be present"),
            state: state.expect("state should be present"),
        }
    })
    .await
    .expect("daemon bootstrap should be bounded")
}

fn assert_bootstrap_contract(messages: &BootstrapPublications) {
    assert_eq!(messages.discovery.qos, QoS::AtLeastOnce);
    assert!(serde_json::from_slice::<serde_json::Value>(&messages.discovery.payload).is_ok());
    assert_eq!(messages.online.qos, QoS::AtLeastOnce);
    assert_eq!(messages.online.payload.as_ref(), b"online");
    assert_eq!(messages.state.qos, QoS::AtMostOnce);
    assert!(!messages.state.retain);
    assert!(serde_json::from_slice::<serde_json::Value>(&messages.state.payload).is_ok());
}

fn observed_at(state: &rumqttc::Publish) -> ObservationTime {
    let document: serde_json::Value =
        serde_json::from_slice(&state.payload).expect("state should be valid JSON");
    ObservationTime::parse(
        document["observed_at"]
            .as_str()
            .expect("state should contain an observation timestamp"),
    )
    .expect("observation timestamp should be RFC 3339")
}

async fn next_topic_publish(
    events: &mut EventLoop,
    topic: &str,
    timeout: Duration,
) -> Option<rumqttc::Publish> {
    tokio::time::timeout(timeout, async {
        loop {
            if let Event::Incoming(Incoming::Publish(publish)) = events
                .poll()
                .await
                .expect("observer event loop should remain connected")
            {
                if publish.topic == topic {
                    return publish;
                }
            }
        }
    })
    .await
    .ok()
}

async fn subscribed_to_topics(
    host: &str,
    port: u16,
    client_id: &str,
    base_topic: &str,
    discovery_topic: &str,
) -> (AsyncClient, EventLoop) {
    let (client, mut events) = observer(host, port, client_id, PASSWORD);
    connect(&mut events).await;
    client
        .subscribe_many([
            SubscribeFilter::new(format!("{base_topic}/#"), QoS::AtLeastOnce),
            SubscribeFilter::new(discovery_topic.to_owned(), QoS::AtLeastOnce),
        ])
        .await
        .expect("observer subscriptions should queue");
    wait_for_subscription(&mut events).await;
    (client, events)
}

async fn wait_for_subscription(events: &mut EventLoop) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                events
                    .poll()
                    .await
                    .expect("observer should remain connected"),
                Event::Incoming(Incoming::SubAck(_))
            ) {
                return;
            }
        }
    })
    .await
    .expect("observer subscription should be acknowledged");
}

async fn assert_retained_lifecycle(
    host: &str,
    port: u16,
    client_id: &str,
    base_topic: &str,
    discovery_topic: &str,
    availability: &[u8],
) {
    let (_client, mut events) =
        subscribed_to_topics(host, port, client_id, base_topic, discovery_topic).await;
    let first = next_publish(&mut events).await;
    let second = next_publish(&mut events).await;
    let retained = [first, second];
    assert!(find_topic(&retained, discovery_topic).retain);
    let retained_availability = find_topic(&retained, &format!("{base_topic}/availability"));
    assert!(retained_availability.retain);
    assert_eq!(retained_availability.payload.as_ref(), availability);
    assert!(
        next_topic_publish(
            &mut events,
            &format!("{base_topic}/state"),
            Duration::from_millis(300),
        )
        .await
        .is_none(),
        "state must not be retained"
    );
}

struct FaultProxy {
    address: SocketAddr,
    enabled: watch::Sender<bool>,
    accept_task: JoinHandle<()>,
}

impl FaultProxy {
    async fn start(upstream_host: &str, upstream_port: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("fault proxy should bind");
        let address = listener
            .local_addr()
            .expect("fault proxy should have a local address");
        let upstream_host: Arc<str> = Arc::from(upstream_host);
        let (enabled, enabled_receiver) = watch::channel(true);
        let accept_task = tokio::spawn(async move {
            loop {
                let (connection, _) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(_) => return,
                };
                let receiver = enabled_receiver.clone();
                if !*receiver.borrow() {
                    drop(connection);
                    continue;
                }
                let host = Arc::clone(&upstream_host);
                tokio::spawn(proxy_connection(connection, host, upstream_port, receiver));
            }
        });
        Self {
            address,
            enabled,
            accept_task,
        }
    }

    fn address(&self) -> SocketAddr {
        self.address
    }

    fn interrupt(&self) {
        self.enabled.send_replace(false);
    }

    fn restore(&self) {
        self.enabled.send_replace(true);
    }

    async fn stop(self) {
        self.enabled.send_replace(false);
        self.accept_task.abort();
        let _ = self.accept_task.await;
    }
}

async fn proxy_connection(
    mut downstream: TcpStream,
    upstream_host: Arc<str>,
    upstream_port: u16,
    mut enabled: watch::Receiver<bool>,
) {
    let connection = TcpStream::connect((upstream_host.as_ref(), upstream_port));
    let mut upstream = tokio::select! {
        result = connection => match result {
            Ok(connection) => connection,
            Err(_) => return,
        },
        () = wait_for_interruption(&mut enabled) => return,
    };
    if !*enabled.borrow() {
        return;
    }
    tokio::select! {
        _ = copy_bidirectional(&mut downstream, &mut upstream) => {}
        () = wait_for_interruption(&mut enabled) => {}
    }
}

async fn wait_for_interruption(enabled: &mut watch::Receiver<bool>) {
    loop {
        if !*enabled.borrow_and_update() {
            return;
        }
        if enabled.changed().await.is_err() {
            return;
        }
    }
}

struct DaemonFixture {
    directory: PathBuf,
    config_path: PathBuf,
}

impl DaemonFixture {
    fn new(device_id: &str, base_topic: &str, proxy: SocketAddr) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "rpi-health-mqtt-daemon-integration-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("daemon fixture directory should be created");
        let password_path = directory.join("mqtt-password");
        fs::write(&password_path, format!("{PASSWORD}\n"))
            .expect("daemon password fixture should be written");
        fs::set_permissions(&password_path, fs::Permissions::from_mode(0o600))
            .expect("daemon password permissions should be restricted");

        let vcgencmd_path = directory.join("vcgencmd");
        fs::write(
            &vcgencmd_path,
            br#"#!/bin/sh
case "$1" in
  get_throttled)
    printf '%s\n' 'throttled=0x0'
    ;;
  measure_temp)
    printf '%s\n' "temp=42.0'C"
    ;;
  *)
    exit 2
    ;;
esac
"#,
        )
        .expect("fake firmware command should be written");
        fs::set_permissions(&vcgencmd_path, fs::Permissions::from_mode(0o700))
            .expect("fake firmware command should be executable");

        let config_path = directory.join("config.toml");
        let config = format!(
            r#"
[device]
id = "{device_id}"
name = "Integration Raspberry Pi"

[mqtt]
host = "{}"
port = {}
client_id = "rpi-health-mqtt-{device_id}"
username = "{USERNAME}"
password_file = "{}"
base_topic = "{base_topic}"
discovery_prefix = "{DAEMON_DISCOVERY_PREFIX}"
keep_alive_seconds = 5

[collector]
interval_seconds = 30
root_filesystem = "/"
vcgencmd_path = "{}"
command_timeout_seconds = 2
"#,
            proxy.ip(),
            proxy.port(),
            password_path.display(),
            vcgencmd_path.display(),
        );
        fs::write(&config_path, config).expect("daemon configuration should be written");
        Self {
            directory,
            config_path,
        }
    }
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn daemon_base_topic(device_id: &str) -> String {
    format!("integration/daemon/{device_id}")
}

fn daemon_discovery_topic(device_id: &str) -> String {
    format!("{DAEMON_DISCOVERY_PREFIX}/device/{device_id}/config")
}

fn spawn_daemon(config_path: &Path) -> Child {
    let binary =
        std::env::var("RPI_HEALTH_MQTT_TEST_BINARY").unwrap_or_else(|_| SERVICE_BINARY.to_owned());
    Command::new(binary)
        .arg("--config")
        .arg(config_path)
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("service binary should start")
}

fn assert_process_running(process: &mut Child) {
    assert!(
        process
            .try_wait()
            .expect("service status should be readable")
            .is_none(),
        "service should remain running"
    );
}

async fn wait_for_output(task: JoinHandle<io::Result<Output>>, timeout: Duration) -> Output {
    tokio::time::timeout(timeout, task)
        .await
        .expect("service process should stop within its deadline")
        .expect("process waiter should not panic")
        .expect("service output should be collected")
}

fn assert_daemon_output_is_safe(output: &Output) {
    assert!(output.stdout.is_empty(), "daemon stdout must remain empty");
    assert!(
        !output
            .stdout
            .windows(PASSWORD.len())
            .any(|value| value == PASSWORD.as_bytes()),
        "daemon stdout must not expose the credential canary"
    );
    assert!(
        !output
            .stderr
            .windows(PASSWORD.len())
            .any(|value| value == PASSWORD.as_bytes()),
        "daemon stderr must not expose the credential canary"
    );
}

async fn subscribed_observer(host: &str, port: u16, client_id: &str) -> (AsyncClient, EventLoop) {
    let (client, mut events) = observer(host, port, client_id, PASSWORD);
    connect(&mut events).await;
    client
        .subscribe_many([
            SubscribeFilter::new(format!("{BASE_TOPIC}/#"), QoS::AtLeastOnce),
            SubscribeFilter::new(DISCOVERY_TOPIC.to_owned(), QoS::AtLeastOnce),
        ])
        .await
        .expect("observer subscriptions should queue");
    (client, events)
}

fn observer(host: &str, port: u16, client_id: &str, password: &str) -> (AsyncClient, EventLoop) {
    let mut options = MqttOptions::new(client_id, host, port);
    options
        .set_clean_session(true)
        .set_keep_alive(Duration::from_secs(5))
        .set_max_packet_size(64 * 1024, 64 * 1024)
        .set_credentials(USERNAME, password);
    AsyncClient::new(options, 10)
}

async fn connect(events: &mut EventLoop) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Event::Incoming(Incoming::ConnAck(acknowledgement)) =
                events.poll().await.expect("observer should connect")
            {
                assert_eq!(acknowledgement.code, ConnectReturnCode::Success);
                return;
            }
        }
    })
    .await
    .expect("observer connection should be bounded");
}

async fn next_publish(events: &mut EventLoop) -> rumqttc::Publish {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Event::Incoming(Incoming::Publish(publish)) = events
                .poll()
                .await
                .expect("observer event loop should remain connected")
            {
                return publish;
            }
        }
    })
    .await
    .expect("expected publication should arrive")
}

fn find_topic<'a>(messages: &'a [rumqttc::Publish], topic: &str) -> &'a rumqttc::Publish {
    messages
        .iter()
        .find(|message| message.topic == topic)
        .unwrap_or_else(|| panic!("expected topic was not received: {topic}"))
}

async fn wait_for_status(status: &mut watch::Receiver<MqttStatus>, expected: MqttStatus) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while *status.borrow_and_update() != expected {
            status
                .changed()
                .await
                .expect("status channel should remain open");
        }
    })
    .await
    .expect("expected MQTT status should arrive");
}

async fn assert_rejected_authentication_is_secret_safe(host: &str, port: u16) {
    let rejected_password = "rejected-test-password";
    let (_client, mut events) = observer(host, port, "observer-rejected", rejected_password);
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.poll().await {
                Ok(Event::Incoming(Incoming::ConnAck(acknowledgement))) => {
                    return format!("connection rejected: {:?}", acknowledgement.code);
                }
                Ok(_) => {}
                Err(error) => return error.to_string(),
            }
        }
    })
    .await
    .expect("authentication rejection should be bounded");
    assert!(!result.contains(rejected_password));
}

fn configuration(host: &str, port: u16, password_path: &Path) -> String {
    format!(
        r#"
[device]
id = "example-pi"
name = "Example Raspberry Pi"

[mqtt]
host = "{host}"
port = {port}
client_id = "rpi-health-mqtt-example-pi-integration"
username = "{USERNAME}"
password_file = "{}"
base_topic = "{BASE_TOPIC}"
discovery_prefix = "homeassistant"
keep_alive_seconds = 5

[collector]
interval_seconds = 5
command_timeout_seconds = 2
"#,
        password_path.display()
    )
}

fn write_password_fixture() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be valid")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rpi-health-mqtt-mqtt-integration-{}-{nonce}",
        std::process::id()
    ));
    fs::write(&path, format!("{PASSWORD}\n")).expect("password fixture should be written");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .expect("password fixture permissions should be restricted");
    path
}
