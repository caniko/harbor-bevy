//! Timeouts are measured from authenticated inbound proof, not frame count.

use std::time::Duration;

use bevy::{prelude::*, time::Real};

use crate::{
    ConnectionLifecyclePlugin, ConnectionLostEvent, DisconnectReason, FromNetwork, NetConfig,
    PeerIdNegotiationState, SendControl, TransportHealth, peer_id::admitted_peer,
};

pub const PEER_READY_RETRY_INTERVAL: Duration = Duration::from_millis(500);
pub const PEER_READY_MAX_ATTEMPTS: u16 = 120;
pub const LIVENESS_PING_INTERVAL: Duration = Duration::from_secs(5);
pub const LIVENESS_STALL_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum LivenessStatus {
    #[default]
    Healthy,
    Stalled,
}

#[derive(Debug, Resource)]
pub struct LivenessWatchdog {
    pub interval: Duration,
    pub stall_timeout: Duration,
    pub last_ping_sent: Duration,
    pub last_ack_received: Option<Duration>,
    pub pending_nonce: Option<u64>,
    pub next_nonce: u64,
    pub missed_intervals: u8,
    pub status: LivenessStatus,
    last_proof: Duration,
    last_remote_nonce: Option<u64>,
}

impl Default for LivenessWatchdog {
    fn default() -> Self {
        Self {
            interval: LIVENESS_PING_INTERVAL,
            stall_timeout: LIVENESS_STALL_TIMEOUT,
            last_ping_sent: Duration::ZERO,
            last_ack_received: None,
            pending_nonce: None,
            next_nonce: 1,
            missed_intervals: 0,
            status: LivenessStatus::Healthy,
            last_proof: Duration::ZERO,
            last_remote_nonce: None,
        }
    }
}

impl LivenessWatchdog {
    pub fn start(&mut self, now: Duration, config: &NetConfig) {
        self.interval = Duration::from_secs(config.liveness_interval_secs.max(1));
        self.stall_timeout = Duration::from_secs(config.liveness_stall_secs.max(1));
        self.last_ping_sent = now;
        self.last_proof = now;
        self.last_ack_received = None;
        self.pending_nonce = None;
        self.missed_intervals = 0;
        self.status = LivenessStatus::Healthy;
    }

    pub fn tick(&mut self, now: Duration) -> Result<Option<LivenessPingEvent>, DisconnectReason> {
        if self.status == LivenessStatus::Stalled {
            return Ok(None);
        }
        let elapsed = now.saturating_sub(self.last_proof);
        let missed = elapsed.as_nanos() / self.interval.as_nanos().max(1);
        self.missed_intervals = u8::try_from(missed).unwrap_or(u8::MAX);
        if elapsed >= self.stall_timeout {
            self.status = LivenessStatus::Stalled;
            return Err(DisconnectReason::LivenessStalled {
                missed_intervals: self.missed_intervals,
                elapsed_secs: elapsed.as_secs_f32(),
            });
        }
        if now.saturating_sub(self.last_ping_sent) < self.interval {
            return Ok(None);
        }
        let nonce = self.next_nonce.max(1);
        self.next_nonce = nonce.wrapping_add(1).max(1);
        self.last_ping_sent = now;
        self.pending_nonce = Some(nonce);
        Ok(Some(LivenessPingEvent { nonce }))
    }

    pub fn acknowledge(&mut self, nonce: u64, now: Duration) -> bool {
        if nonce == 0 || self.pending_nonce != Some(nonce) {
            return false;
        }
        self.pending_nonce = None;
        self.last_ack_received = Some(now);
        self.refresh(now);
        true
    }

    /// Return false for a zero nonce or replay. Replays may still be ACKed.
    pub fn receive_ping(&mut self, nonce: u64, now: Duration) -> bool {
        if nonce == 0
            || self.last_remote_nonce.is_some_and(|last| {
                let advance = nonce.wrapping_sub(last);
                advance == 0 || advance >= (1_u64 << 63)
            })
        {
            return false;
        }
        self.last_remote_nonce = Some(nonce);
        self.refresh(now);
        true
    }

    fn refresh(&mut self, now: Duration) {
        self.last_proof = self.last_proof.max(now);
        self.missed_intervals = 0;
    }
}

macro_rules! ping_event {
    ($name:ident) => {
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            Event,
            rkyv::Archive,
            rkyv::Serialize,
            rkyv::Deserialize,
        )]
        pub struct $name {
            pub nonce: u64,
        }
    };
}

ping_event!(LivenessPingEvent);
ping_event!(LivenessPingAckEvent);

pub struct LivenessPlugin;

impl Plugin for LivenessPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<ConnectionLifecyclePlugin>() {
            app.add_plugins(ConnectionLifecyclePlugin);
        }
        app.init_resource::<PeerIdNegotiationState>()
            .init_resource::<NetConfig>()
            .init_resource::<LivenessWatchdog>()
            .add_observer(
                |_: On<crate::SessionCleanupRequested>, mut clock: ResMut<LivenessWatchdog>| {
                    *clock = LivenessWatchdog::default();
                },
            )
            .add_systems(OnEnter(TransportHealth::Healthy), start)
            .add_systems(Update, tick.run_if(in_state(TransportHealth::Healthy)))
            .add_observer(ping)
            .add_observer(ack);
    }
}

fn start(time: Res<Time<Real>>, config: Res<NetConfig>, mut watchdog: ResMut<LivenessWatchdog>) {
    watchdog.start(time.elapsed(), &config);
}

fn tick(
    time: Res<Time<Real>>,
    peers: Res<PeerIdNegotiationState>,
    mut watchdog: ResMut<LivenessWatchdog>,
    mut commands: Commands,
) {
    let Some(seats) = peers.assignment() else {
        return;
    };
    match watchdog.tick(time.elapsed()) {
        Ok(Some(ping)) => commands.trigger(SendControl::new(seats.remote.clone(), ping)),
        Ok(None) => {}
        Err(reason) => commands.trigger(ConnectionLostEvent(reason)),
    }
}

fn ping(
    ping: On<FromNetwork<LivenessPingEvent>>,
    peers: Res<PeerIdNegotiationState>,
    time: Res<Time<Real>>,
    health: Res<State<TransportHealth>>,
    mut watchdog: ResMut<LivenessWatchdog>,
    mut commands: Commands,
) {
    let Some(seats) = admitted_peer(&peers, ping.from.as_ref()) else {
        return;
    };
    if *health.get() == TransportHealth::Healthy && ping.inner.nonce != 0 {
        watchdog.receive_ping(ping.inner.nonce, time.elapsed());
        commands.trigger(SendControl::new(
            seats.remote.clone(),
            LivenessPingAckEvent {
                nonce: ping.inner.nonce,
            },
        ));
    }
}

fn ack(
    ack: On<FromNetwork<LivenessPingAckEvent>>,
    peers: Res<PeerIdNegotiationState>,
    time: Res<Time<Real>>,
    mut watchdog: ResMut<LivenessWatchdog>,
) {
    if admitted_peer(&peers, ack.from.as_ref()).is_some() {
        watchdog.acknowledge(ack.inner.nonce, time.elapsed());
    }
}
