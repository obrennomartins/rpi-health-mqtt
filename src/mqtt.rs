//! Bounded, reconnecting MQTT publication.
//!
//! The supervisor owns the MQTT event loop and never queues periodic history.
//! State arrives through a [`watch`] slot, so an outage retains only the newest
//! observation. Every connection publishes retained Discovery, retained
//! availability, and then a newly requested non-retained state in that order.

use std::{
    fmt,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rumqttc::{
    AsyncClient, ConnectReturnCode, Event, EventLoop, Incoming, LastWill, MqttOptions,
    NetworkOptions, Outgoing, QoS,
};
use thiserror::Error;
use tokio::sync::watch;

use crate::{config::MqttConfig, config::MqttCredentials, discovery::DiscoveryMessage};

const REQUEST_CHANNEL_CAPACITY: usize = 16;
const MAX_PACKET_BYTES: usize = 64 * 1024;
const MAX_INFLIGHT: u16 = 4;
const CONNECT_TIMEOUT_SECONDS: u64 = 5;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const RETRY_SECONDS: [u64; 7] = [1, 2, 4, 8, 16, 30, 60];

/// Immutable, non-secret MQTT settings used by the supervisor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MqttSettings {
    host: String,
    port: u16,
    client_id: String,
    keep_alive: Duration,
    state_topic: String,
    availability_topic: String,
    discovery_topic: String,
}

impl MqttSettings {
    /// Builds publication settings from validated configuration.
    #[must_use]
    pub fn new(config: &MqttConfig, device_id: &str) -> Self {
        Self {
            host: config.host().to_owned(),
            port: config.port(),
            client_id: config.client_id().to_owned(),
            keep_alive: config.keep_alive(),
            state_topic: config.state_topic(),
            availability_topic: config.availability_topic(),
            discovery_topic: config.discovery_topic(device_id),
        }
    }

    /// Returns the periodic state topic.
    #[must_use]
    pub fn state_topic(&self) -> &str {
        &self.state_topic
    }

    /// Returns the retained availability topic.
    #[must_use]
    pub fn availability_topic(&self) -> &str {
        &self.availability_topic
    }

    /// Returns the retained Device Discovery topic.
    #[must_use]
    pub fn discovery_topic(&self) -> &str {
        &self.discovery_topic
    }

    fn options(&self, credentials: &MqttCredentials) -> MqttOptions {
        let mut options = MqttOptions::new(&self.client_id, &self.host, self.port);
        options
            .set_clean_session(true)
            .set_keep_alive(self.keep_alive)
            .set_credentials(credentials.username(), credentials.expose_password())
            .set_last_will(LastWill::new(
                &self.availability_topic,
                b"offline",
                QoS::AtLeastOnce,
                true,
            ))
            .set_max_packet_size(MAX_PACKET_BYTES, MAX_PACKET_BYTES)
            .set_request_channel_capacity(REQUEST_CHANNEL_CAPACITY)
            .set_inflight(MAX_INFLIGHT);
        options
    }
}

/// A serialized observation tagged with the request that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateMessage {
    /// Monotonic collection request identifier.
    pub request_id: u64,
    /// Compact JSON state payload.
    pub payload: Arc<[u8]>,
}

impl StateMessage {
    /// Creates a tagged serialized observation.
    #[must_use]
    pub fn new(request_id: u64, payload: impl Into<Arc<[u8]>>) -> Self {
        Self {
            request_id,
            payload: payload.into(),
        }
    }
}

/// Observable MQTT lifecycle status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MqttStatus {
    /// No acknowledged connection is active.
    Disconnected,
    /// Discovery and availability are being established.
    Bootstrapping,
    /// Bootstrap completed and state can be published.
    Online,
    /// Graceful shutdown is in progress.
    ShuttingDown,
    /// The supervisor has stopped.
    Stopped,
}

/// Fatal internal MQTT supervisor failures.
///
/// Network and broker failures are deliberately absent: the supervisor retries
/// those conditions until shutdown.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum MqttError {
    /// The bounded MQTT client request channel unexpectedly rejected a control message.
    #[error("the MQTT control queue is unavailable")]
    ControlQueueUnavailable,
}

/// Exponential retry delay with bounded jitter.
#[derive(Clone, Debug, Default)]
pub struct RetryBackoff {
    attempt: usize,
}

impl RetryBackoff {
    /// Creates a retry sequence starting at one second.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the next delay using `jitter_unit` in the inclusive range 0–1.
    ///
    /// Values outside that range are clamped. The resulting jitter is ±20%
    /// around the base sequence `1, 2, 4, 8, 16, 30, 60` seconds.
    #[must_use]
    pub fn next_delay(&mut self, jitter_unit: f64) -> Duration {
        let index = self.attempt.min(RETRY_SECONDS.len() - 1);
        self.attempt = self.attempt.saturating_add(1);
        let unit = if jitter_unit.is_finite() {
            jitter_unit.clamp(0.0, 1.0)
        } else {
            0.5
        };
        Duration::from_secs_f64(RETRY_SECONDS[index] as f64 * (0.8 + 0.4 * unit))
    }

    /// Restarts the sequence after a successful connection acknowledgement.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// Long-lived MQTT event-loop owner.
pub struct MqttSupervisor {
    settings: MqttSettings,
    credentials: MqttCredentials,
    discovery_payload: Arc<[u8]>,
    states: watch::Receiver<Option<StateMessage>>,
    collection_requests: watch::Sender<u64>,
    shutdown: watch::Receiver<bool>,
    status: watch::Sender<MqttStatus>,
}

impl MqttSupervisor {
    /// Creates a supervisor and a status receiver.
    ///
    /// `collection_requests` is a latest-value request slot. A collector must
    /// tag its response with the received identifier and replace `states`.
    #[must_use]
    pub fn new(
        settings: MqttSettings,
        credentials: MqttCredentials,
        discovery: &DiscoveryMessage,
        states: watch::Receiver<Option<StateMessage>>,
        collection_requests: watch::Sender<u64>,
        shutdown: watch::Receiver<bool>,
    ) -> (Self, watch::Receiver<MqttStatus>) {
        let (status, status_receiver) = watch::channel(MqttStatus::Disconnected);
        (
            Self {
                settings,
                credentials,
                discovery_payload: Arc::from(discovery.payload.as_bytes()),
                states,
                collection_requests,
                shutdown,
                status,
            },
            status_receiver,
        )
    }

    /// Runs until shutdown, reconnecting indefinitely after transport failures.
    ///
    /// # Errors
    ///
    /// Returns only for an unexpected bounded-control-queue failure.
    pub async fn run(mut self) -> Result<(), MqttError> {
        let options = self.settings.options(&self.credentials);
        let (client, mut event_loop) = AsyncClient::new(options, REQUEST_CHANNEL_CAPACITY);
        let mut network_options = NetworkOptions::new();
        network_options.set_connection_timeout(CONNECT_TIMEOUT_SECONDS);
        event_loop.set_network_options(network_options);

        let mut protocol = BootstrapMachine::default();
        let mut backoff = RetryBackoff::new();
        let mut jitter = Jitter::new();
        let mut next_request_id = 0_u64;

        loop {
            if *self.shutdown.borrow() {
                return self
                    .graceful_shutdown(&client, &mut event_loop, &mut protocol)
                    .await;
            }

            tokio::select! {
                biased;
                changed = self.shutdown.changed() => {
                    if changed.is_err() || *self.shutdown.borrow() {
                        return self.graceful_shutdown(&client, &mut event_loop, &mut protocol).await;
                    }
                }
                event = event_loop.poll() => {
                    match event {
                        Ok(event) => {
                            if let Event::Incoming(Incoming::ConnAck(ref acknowledgement)) = event {
                                if acknowledgement.code == ConnectReturnCode::Success {
                                    backoff.reset();
                                    next_request_id = next_request_id.saturating_add(1);
                                    self.states.borrow_and_update();
                                    self.collection_requests.send_replace(next_request_id);
                                    self.status.send_replace(MqttStatus::Bootstrapping);
                                    let action = protocol.connected(next_request_id);
                                    self.perform(&client, action, None)?;
                                    continue;
                                }
                            }
                            self.handle_event(&client, &mut protocol, event)?;
                        }
                        Err(_) => {
                            protocol.connection_lost();
                            self.status.send_replace(MqttStatus::Disconnected);
                            let delay = backoff.next_delay(jitter.next_unit());
                            tokio::select! {
                                () = tokio::time::sleep(delay) => {}
                                changed = self.shutdown.changed() => {
                                    if changed.is_err() || *self.shutdown.borrow() {
                                        self.status.send_replace(MqttStatus::Stopped);
                                        return Ok(());
                                    }
                                }
                            }
                        }
                    }
                }
                changed = self.states.changed(), if protocol.accepts_state() => {
                    if changed.is_ok() {
                        self.publish_current_state(&client, &mut protocol)?;
                    }
                }
            }

            if protocol.just_became_online() {
                self.status.send_replace(MqttStatus::Online);
                self.publish_current_state(&client, &mut protocol)?;
            } else if protocol.is_online() {
                self.status.send_replace(MqttStatus::Online);
            }
        }
    }

    fn handle_event(
        &mut self,
        client: &AsyncClient,
        protocol: &mut BootstrapMachine,
        event: Event,
    ) -> Result<(), MqttError> {
        let action = match event {
            Event::Outgoing(Outgoing::Publish(packet_id)) => protocol.outgoing_publish(packet_id),
            Event::Incoming(Incoming::PubAck(acknowledgement)) => {
                protocol.publish_acknowledged(acknowledgement.pkid)
            }
            Event::Outgoing(Outgoing::Disconnect) => protocol.disconnected(),
            _ => None,
        };
        if let Some(action) = action {
            self.perform(client, action, None)?;
        }
        Ok(())
    }

    fn publish_current_state(
        &mut self,
        client: &AsyncClient,
        protocol: &mut BootstrapMachine,
    ) -> Result<(), MqttError> {
        let message = self.states.borrow_and_update().clone();
        if let Some(message) = message {
            if let Some(action) = protocol.state_available(message.request_id) {
                self.perform(client, action, Some(&message.payload))?;
            }
        }
        Ok(())
    }

    fn perform(
        &self,
        client: &AsyncClient,
        action: ProtocolAction,
        state_payload: Option<&[u8]>,
    ) -> Result<(), MqttError> {
        let result = match action {
            ProtocolAction::PublishDiscovery => client.try_publish(
                &self.settings.discovery_topic,
                QoS::AtLeastOnce,
                true,
                self.discovery_payload.as_ref(),
            ),
            ProtocolAction::PublishOnline => client.try_publish(
                &self.settings.availability_topic,
                QoS::AtLeastOnce,
                true,
                b"online".as_slice(),
            ),
            ProtocolAction::PublishState => client.try_publish(
                &self.settings.state_topic,
                QoS::AtMostOnce,
                false,
                state_payload.unwrap_or_default(),
            ),
            ProtocolAction::PublishOffline => client.try_publish(
                &self.settings.availability_topic,
                QoS::AtLeastOnce,
                true,
                b"offline".as_slice(),
            ),
            ProtocolAction::Disconnect => client.try_disconnect(),
        };
        result.map_err(|_| MqttError::ControlQueueUnavailable)
    }

    async fn graceful_shutdown(
        &mut self,
        client: &AsyncClient,
        event_loop: &mut EventLoop,
        protocol: &mut BootstrapMachine,
    ) -> Result<(), MqttError> {
        self.status.send_replace(MqttStatus::ShuttingDown);
        let Some(action) = protocol.shutdown() else {
            self.status.send_replace(MqttStatus::Stopped);
            return Ok(());
        };
        self.perform(client, action, None)?;

        let sequence = async {
            loop {
                match event_loop.poll().await {
                    Ok(event) => {
                        let stopped = matches!(event, Event::Outgoing(Outgoing::Disconnect));
                        self.handle_event(client, protocol, event)?;
                        if stopped || protocol.is_stopped() {
                            return Ok::<(), MqttError>(());
                        }
                    }
                    Err(_) => return Ok::<(), MqttError>(()),
                }
            }
        };
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, sequence).await;
        self.status.send_replace(MqttStatus::Stopped);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Phase {
    #[default]
    Disconnected,
    DiscoveryQueued,
    DiscoveryAwaitAck(u16),
    OnlineQueued,
    OnlineAwaitAck(u16),
    AwaitFreshState,
    StateQueued,
    Online,
    OfflineQueued,
    OfflineAwaitAck(u16),
    DisconnectQueued,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProtocolAction {
    PublishDiscovery,
    PublishOnline,
    PublishState,
    PublishOffline,
    Disconnect,
}

#[derive(Clone, Debug, Default)]
struct BootstrapMachine {
    phase: Phase,
    required_request: u64,
    last_state_request: u64,
    became_online: bool,
}

impl BootstrapMachine {
    fn connected(&mut self, required_request: u64) -> ProtocolAction {
        self.required_request = required_request;
        self.became_online = false;
        self.phase = Phase::DiscoveryQueued;
        ProtocolAction::PublishDiscovery
    }

    fn outgoing_publish(&mut self, packet_id: u16) -> Option<ProtocolAction> {
        match self.phase {
            Phase::DiscoveryQueued if packet_id != 0 => {
                self.phase = Phase::DiscoveryAwaitAck(packet_id);
            }
            Phase::OnlineQueued if packet_id != 0 => {
                self.phase = Phase::OnlineAwaitAck(packet_id);
            }
            Phase::StateQueued if packet_id == 0 => {
                self.phase = Phase::Online;
                self.became_online = true;
            }
            Phase::OfflineQueued if packet_id != 0 => {
                self.phase = Phase::OfflineAwaitAck(packet_id);
            }
            _ => {}
        }
        None
    }

    fn publish_acknowledged(&mut self, packet_id: u16) -> Option<ProtocolAction> {
        match self.phase {
            Phase::DiscoveryAwaitAck(expected) if expected == packet_id => {
                self.phase = Phase::OnlineQueued;
                Some(ProtocolAction::PublishOnline)
            }
            Phase::OnlineAwaitAck(expected) if expected == packet_id => {
                self.phase = Phase::AwaitFreshState;
                None
            }
            Phase::OfflineAwaitAck(expected) if expected == packet_id => {
                self.phase = Phase::DisconnectQueued;
                Some(ProtocolAction::Disconnect)
            }
            _ => None,
        }
    }

    fn state_available(&mut self, request_id: u64) -> Option<ProtocolAction> {
        let allowed = match self.phase {
            Phase::AwaitFreshState => request_id >= self.required_request,
            Phase::Online => request_id > self.last_state_request,
            _ => false,
        };
        if allowed {
            self.last_state_request = request_id;
            self.became_online = false;
            self.phase = Phase::StateQueued;
            Some(ProtocolAction::PublishState)
        } else {
            None
        }
    }

    fn shutdown(&mut self) -> Option<ProtocolAction> {
        match self.phase {
            Phase::Disconnected | Phase::Stopped => {
                self.phase = Phase::Stopped;
                None
            }
            _ => {
                self.phase = Phase::OfflineQueued;
                Some(ProtocolAction::PublishOffline)
            }
        }
    }

    fn disconnected(&mut self) -> Option<ProtocolAction> {
        self.phase = Phase::Stopped;
        None
    }

    fn connection_lost(&mut self) {
        self.phase = Phase::Disconnected;
        self.became_online = false;
    }

    fn accepts_state(&self) -> bool {
        matches!(self.phase, Phase::AwaitFreshState | Phase::Online)
    }

    fn is_online(&self) -> bool {
        self.phase == Phase::Online
    }

    fn just_became_online(&mut self) -> bool {
        std::mem::take(&mut self.became_online)
    }

    fn is_stopped(&self) -> bool {
        self.phase == Phase::Stopped
    }
}

struct Jitter(u64);

impl Jitter {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seed = u64::try_from(nanos).unwrap_or(u64::MAX) ^ u64::from(std::process::id());
        Self(seed.max(1))
    }

    fn next_unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as f64 / u64::MAX as f64
    }
}

impl fmt::Debug for MqttSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MqttSupervisor")
            .field("settings", &self.settings)
            .field("credentials", &"[REDACTED]")
            .field("discovery_payload_bytes", &self.discovery_payload.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_requires_acknowledged_discovery_and_online_before_fresh_state() {
        let mut machine = BootstrapMachine::default();
        assert_eq!(machine.connected(7), ProtocolAction::PublishDiscovery);
        assert_eq!(machine.state_available(7), None);

        machine.outgoing_publish(10);
        assert_eq!(machine.publish_acknowledged(9), None);
        assert_eq!(
            machine.publish_acknowledged(10),
            Some(ProtocolAction::PublishOnline)
        );
        machine.outgoing_publish(11);
        assert_eq!(machine.publish_acknowledged(11), None);
        assert_eq!(machine.phase, Phase::AwaitFreshState);
        assert_eq!(machine.state_available(6), None);
        assert_eq!(
            machine.state_available(7),
            Some(ProtocolAction::PublishState)
        );
        machine.outgoing_publish(0);
        assert!(machine.is_online());
    }

    #[test]
    fn periodic_backpressure_keeps_only_a_newer_state() {
        let mut machine = BootstrapMachine {
            phase: Phase::Online,
            last_state_request: 20,
            ..BootstrapMachine::default()
        };
        assert_eq!(machine.state_available(20), None);
        assert_eq!(
            machine.state_available(21),
            Some(ProtocolAction::PublishState)
        );
        assert_eq!(machine.state_available(22), None);
        machine.outgoing_publish(0);
        assert_eq!(
            machine.state_available(22),
            Some(ProtocolAction::PublishState)
        );
    }

    #[test]
    fn every_reconnection_restarts_the_bootstrap() {
        let mut machine = BootstrapMachine::default();
        machine.connected(1);
        machine.outgoing_publish(1);
        machine.publish_acknowledged(1);
        machine.outgoing_publish(2);
        machine.publish_acknowledged(2);
        machine.state_available(1);
        machine.outgoing_publish(0);
        machine.connection_lost();

        assert_eq!(machine.connected(2), ProtocolAction::PublishDiscovery);
        assert_eq!(machine.state_available(1), None);
        assert_eq!(machine.phase, Phase::DiscoveryQueued);
    }

    #[test]
    fn graceful_shutdown_waits_for_offline_ack_before_disconnect() {
        let mut machine = BootstrapMachine {
            phase: Phase::Online,
            ..BootstrapMachine::default()
        };
        assert_eq!(machine.shutdown(), Some(ProtocolAction::PublishOffline));
        machine.outgoing_publish(31);
        assert_eq!(machine.publish_acknowledged(30), None);
        assert_eq!(
            machine.publish_acknowledged(31),
            Some(ProtocolAction::Disconnect)
        );
        assert_eq!(machine.phase, Phase::DisconnectQueued);
        machine.disconnected();
        assert!(machine.is_stopped());
    }

    #[test]
    fn disconnected_shutdown_stops_without_publication() {
        let mut machine = BootstrapMachine::default();
        assert_eq!(machine.shutdown(), None);
        assert!(machine.is_stopped());
    }

    #[test]
    fn backoff_sequence_is_capped_jittered_and_resettable() {
        let mut backoff = RetryBackoff::new();
        let low: Vec<_> = (0..8).map(|_| backoff.next_delay(0.0)).collect();
        assert_eq!(
            low,
            [0.8, 1.6, 3.2, 6.4, 12.8, 24.0, 48.0, 48.0].map(Duration::from_secs_f64)
        );
        backoff.reset();
        assert_eq!(backoff.next_delay(1.0), Duration::from_millis(1_200));
        assert_eq!(backoff.next_delay(f64::NAN), Duration::from_secs(2));
    }

    #[test]
    fn jitter_generator_stays_in_the_unit_interval() {
        let mut jitter = Jitter(42);
        for _ in 0..1_000 {
            assert!((0.0..=1.0).contains(&jitter.next_unit()));
        }
    }

    #[test]
    fn state_message_owns_a_bounded_shareable_payload() {
        let message = StateMessage::new(9, Arc::<[u8]>::from(b"{}".as_slice()));
        assert_eq!(message.request_id, 9);
        assert_eq!(message.payload.as_ref(), b"{}");
    }
}
