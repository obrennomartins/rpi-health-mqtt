//! Authenticated real-broker validation for the MQTT publication contract.

#![cfg(target_os = "linux")]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rpi_health_mqtt::{
    config::{Config, MqttCredentials},
    discovery::{build_discovery_message, DiscoverySettings},
    mqtt::{MqttSettings, MqttStatus, MqttSupervisor, StateMessage},
};
use rumqttc::{
    AsyncClient, ConnectReturnCode, Event, EventLoop, Incoming, MqttOptions, QoS, SubscribeFilter,
};
use tokio::sync::watch;

const USERNAME: &str = "monitor-example";
const PASSWORD: &str = "test-only-password";
const BASE_TOPIC: &str = "example/monitor/example-pi";
const DISCOVERY_TOPIC: &str = "homeassistant/device/example-pi/config";

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
