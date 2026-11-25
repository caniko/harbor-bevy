use std::{collections::VecDeque, time::Duration};

use bevy::{prelude::*, time::Real};
use thespan::NetworkCommand;
use thespan_bevy::{
    GossipSubscribeFailedEvent, GossipSubscribedEvent, NetworkResource, RawGossipMessageEvent,
};

use crate::{
    ConnectionLostEvent, DisconnectReason, FromNetwork, GossipTopic, NetConfig, OutboundSeq,
    PEER_READY_MAX_ATTEMPTS, PEER_READY_RETRY_INTERVAL, PeerIdNegotiationState, PlayerId,
    SendControl, SessionCleanupRequested, SessionEpoch, peer_id::admitted_peer,
};

pub const GOSSIP_QUEUE_CAPACITY: usize = 128;
const MAX_GOSSIP_PAYLOAD: usize = thespan::GAMEPLAY_MESSAGE_MAX_BYTES - 128;

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct GossipEnvelope {
    pub kind: u16,
    pub player_id: PlayerId,
    pub seq: OutboundSeq,
    pub epoch: SessionEpoch,
    pub payload: Vec<u8>,
}

impl GossipEnvelope {
    /// Stable application broadcast identifier, independent of archive layout.
    pub fn broadcast_id(&self) -> [u8; 20] {
        let mut hash = blake3::Hasher::new();
        hash.update(&1_u16.to_le_bytes());
        hash.update(&self.kind.to_le_bytes());
        hash.update(&self.epoch.match_id);
        hash.update(&self.epoch.battle_number.0.to_le_bytes());
        hash.update(&self.epoch.round_number.0.to_le_bytes());
        hash.update(&[self.player_id.0]);
        hash.update(&self.seq.raw().to_le_bytes());
        hash.update(blake3::hash(&self.payload).as_bytes());
        let digest = hash.finalize();
        std::array::from_fn(|index| digest.as_bytes()[index])
    }

    fn key(&self) -> (u16, PlayerId, OutboundSeq, SessionEpoch) {
        (self.kind, self.player_id, self.seq, self.epoch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct GossipGameAction<E> {
    pub action: E,
    pub effects: Vec<E>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum GossipReadiness {
    #[default]
    Disabled,
    Subscribing {
        topic: GossipTopic,
    },
    LocalSubscribed {
        topic: GossipTopic,
    },
    Ready {
        topic: GossipTopic,
    },
    AwaitingStateSyncAck {
        topic: GossipTopic,
    },
}

impl GossipReadiness {
    pub fn topic(&self) -> Option<&GossipTopic> {
        match self {
            Self::Disabled => None,
            Self::Subscribing { topic }
            | Self::LocalSubscribed { topic }
            | Self::Ready { topic }
            | Self::AwaitingStateSyncAck { topic } => Some(topic),
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct MatchGossipReady {
    pub match_id: [u8; 16],
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct MatchGossipReadyAck {
    pub match_id: [u8; 16],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Event, thiserror::Error)]
pub enum GossipQueueError {
    #[error("No subscription for this match")]
    WrongMatch,
    #[error("Pre-ready gossip queue is full")]
    Full,
    #[error("Gossip payload exceeds the transport limit")]
    Oversized,
}

#[derive(Default, Resource)]
pub struct GossipState {
    readiness: GossipReadiness,
    match_id: Option<[u8; 16]>,
    await_sync: bool,
    remote_ready: bool,
    retry: Option<(Duration, u16)>,
    outbound: VecDeque<GossipEnvelope>,
    inbound: VecDeque<GossipEnvelope>,
    early_inbound_dropped: u64,
}

impl GossipState {
    pub fn readiness(&self) -> &GossipReadiness {
        &self.readiness
    }

    /// Oldest inbound envelopes evicted before the current subscription was ready.
    pub fn early_inbound_dropped_count(&self) -> u64 {
        self.early_inbound_dropped
    }

    pub fn subscribe(&mut self, config: &NetConfig, match_id: [u8; 16], await_sync: bool) {
        *self = Self {
            readiness: GossipReadiness::Subscribing {
                topic: config.match_topic(&match_id),
            },
            match_id: Some(match_id),
            await_sync,
            ..Default::default()
        };
    }

    pub fn local_subscribed(&mut self, topic: &GossipTopic) -> bool {
        if !matches!(&self.readiness, GossipReadiness::Subscribing { topic: pending } if pending == topic)
        {
            return false;
        }
        self.readiness = if self.await_sync {
            GossipReadiness::AwaitingStateSyncAck {
                topic: topic.clone(),
            }
        } else {
            GossipReadiness::LocalSubscribed {
                topic: topic.clone(),
            }
        };
        true
    }

    pub fn state_sync_acknowledged(&mut self) -> bool {
        self.await_sync = false;
        let GossipReadiness::AwaitingStateSyncAck { topic } = &self.readiness else {
            return false;
        };
        self.readiness = GossipReadiness::LocalSubscribed {
            topic: topic.clone(),
        };
        true
    }

    pub fn acknowledge(&mut self, match_id: [u8; 16]) -> bool {
        let GossipReadiness::LocalSubscribed { topic } = &self.readiness else {
            return false;
        };
        if self.match_id != Some(match_id) {
            return false;
        }
        self.readiness = GossipReadiness::Ready {
            topic: topic.clone(),
        };
        self.retry = None;
        true
    }

    pub fn outbound(
        &mut self,
        envelope: GossipEnvelope,
    ) -> Result<Option<GossipEnvelope>, GossipQueueError> {
        if self.match_id != Some(envelope.epoch.match_id) {
            return Err(GossipQueueError::WrongMatch);
        }
        if envelope.payload.len() > MAX_GOSSIP_PAYLOAD {
            return Err(GossipQueueError::Oversized);
        }
        if matches!(self.readiness, GossipReadiness::Ready { .. }) {
            return Ok(Some(envelope));
        }
        if self.outbound.len() == GOSSIP_QUEUE_CAPACITY {
            return Err(GossipQueueError::Full);
        }
        self.outbound.push_back(envelope);
        Ok(None)
    }

    pub fn inbound(&mut self, envelope: GossipEnvelope) -> Option<GossipEnvelope> {
        if self.match_id != Some(envelope.epoch.match_id)
            || envelope.payload.len() > MAX_GOSSIP_PAYLOAD
        {
            return None;
        }
        if matches!(self.readiness, GossipReadiness::Ready { .. }) {
            return Some(envelope);
        }
        // ponytail: scan at most 128 items; index keys if this bound grows.
        if !self
            .inbound
            .iter()
            .any(|pending| pending.key() == envelope.key())
        {
            if self.inbound.len() == GOSSIP_QUEUE_CAPACITY {
                self.inbound.pop_front();
                self.early_inbound_dropped = self.early_inbound_dropped.saturating_add(1);
            }
            self.inbound.push_back(envelope);
        }
        None
    }

    pub fn drain_ready(&mut self) -> (Vec<GossipEnvelope>, Vec<GossipEnvelope>) {
        if matches!(self.readiness, GossipReadiness::Ready { .. }) {
            (
                self.outbound.drain(..).collect(),
                self.inbound.drain(..).collect(),
            )
        } else {
            (vec![], vec![])
        }
    }
}

#[derive(Debug, Event)]
pub struct BeginMatchGossip {
    pub await_state_sync_ack: bool,
}

#[derive(Debug, Event)]
pub struct BroadcastGossip(pub GossipEnvelope);

#[derive(Debug, Event)]
pub struct GossipReceived(pub GossipEnvelope);

pub struct GossipPlugin;

impl Plugin for GossipPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NetConfig>()
            .init_resource::<PeerIdNegotiationState>()
            .init_resource::<GossipState>()
            .add_observer(begin)
            .add_observer(subscribed)
            .add_observer(subscription_failed)
            .add_observer(remote_ready)
            .add_observer(remote_ack)
            .add_observer(sync_ack)
            .add_observer(broadcast)
            .add_observer(receive)
            .add_observer(
                |_: On<SessionCleanupRequested>, mut gossip: ResMut<GossipState>| {
                    *gossip = GossipState::default();
                },
            )
            .add_systems(Update, retry_ready);
    }
}

fn begin(
    event: On<BeginMatchGossip>,
    peers: Res<PeerIdNegotiationState>,
    config: Res<NetConfig>,
    net: Option<Res<NetworkResource>>,
    mut gossip: ResMut<GossipState>,
    mut commands: Commands,
) {
    let (Some(seats), Some(net)) = (peers.assignment(), net.filter(|net| net.gossipsub_enabled))
    else {
        return;
    };
    if let Err(error) = net.send(NetworkCommand::Subscribe {
        topic: config.match_topic(&seats.match_id),
    }) {
        commands.trigger(ConnectionLostEvent(DisconnectReason::Error(
            error.to_string(),
        )));
        return;
    }
    gossip.subscribe(&config, seats.match_id, event.await_state_sync_ack);
}

fn subscribed(
    event: On<GossipSubscribedEvent>,
    time: Res<Time<Real>>,
    peers: Res<PeerIdNegotiationState>,
    mut gossip: ResMut<GossipState>,
    mut commands: Commands,
) {
    if gossip.local_subscribed(&event.topic)
        && let Some(seats) = peers.assignment()
    {
        announce(&mut gossip, seats, time.elapsed(), &mut commands);
    }
}

fn subscription_failed(
    event: On<GossipSubscribeFailedEvent>,
    mut gossip: ResMut<GossipState>,
    mut commands: Commands,
) {
    if matches!(&gossip.readiness, GossipReadiness::Subscribing { topic } if *topic == event.topic)
    {
        *gossip = GossipState::default();
        commands.trigger(ConnectionLostEvent(DisconnectReason::Error(
            event.reason.clone(),
        )));
    }
}

fn sync_ack(
    _: On<crate::StateSyncSettled>,
    time: Res<Time<Real>>,
    peers: Res<PeerIdNegotiationState>,
    mut gossip: ResMut<GossipState>,
    mut commands: Commands,
) {
    if gossip.state_sync_acknowledged()
        && let Some(seats) = peers.assignment()
    {
        announce(&mut gossip, seats, time.elapsed(), &mut commands);
    }
}

fn announce(
    gossip: &mut GossipState,
    seats: &crate::PeerSlotsAssigned,
    now: Duration,
    commands: &mut Commands,
) {
    if !matches!(gossip.readiness, GossipReadiness::LocalSubscribed { .. }) {
        return;
    }
    gossip.retry = Some((now + PEER_READY_RETRY_INTERVAL, 1));
    commands.trigger(SendControl::new(
        seats.remote.clone(),
        MatchGossipReady {
            match_id: seats.match_id,
        },
    ));
    if gossip.remote_ready {
        commands.trigger(SendControl::new(
            seats.remote.clone(),
            MatchGossipReadyAck {
                match_id: seats.match_id,
            },
        ));
    }
}

fn retry_ready(
    time: Res<Time<Real>>,
    peers: Res<PeerIdNegotiationState>,
    mut gossip: ResMut<GossipState>,
    mut commands: Commands,
) {
    let (Some((due, attempts)), Some(seats)) = (gossip.retry, peers.assignment()) else {
        return;
    };
    if time.elapsed() < due {
        return;
    }
    if attempts >= PEER_READY_MAX_ATTEMPTS {
        gossip.retry = None;
        commands.trigger(ConnectionLostEvent(DisconnectReason::PeerReadyTimeout {
            attempts,
        }));
    } else {
        gossip.retry = Some((time.elapsed() + PEER_READY_RETRY_INTERVAL, attempts + 1));
        commands.trigger(SendControl::new(
            seats.remote.clone(),
            MatchGossipReady {
                match_id: seats.match_id,
            },
        ));
    }
}

fn remote_ready(
    event: On<FromNetwork<MatchGossipReady>>,
    peers: Res<PeerIdNegotiationState>,
    mut gossip: ResMut<GossipState>,
    mut commands: Commands,
) {
    let Some(seats) = admitted_peer(&peers, event.from.as_ref()) else {
        return;
    };
    if gossip.match_id != Some(event.inner.match_id) {
        return;
    }
    gossip.remote_ready = true;
    if matches!(
        gossip.readiness,
        GossipReadiness::LocalSubscribed { .. } | GossipReadiness::Ready { .. }
    ) {
        commands.trigger(SendControl::new(
            seats.remote.clone(),
            MatchGossipReadyAck {
                match_id: seats.match_id,
            },
        ));
    }
}

fn remote_ack(
    event: On<FromNetwork<MatchGossipReadyAck>>,
    peers: Res<PeerIdNegotiationState>,
    net: Option<Res<NetworkResource>>,
    mut gossip: ResMut<GossipState>,
    mut commands: Commands,
) {
    if admitted_peer(&peers, event.from.as_ref()).is_none()
        || !gossip.acknowledge(event.inner.match_id)
    {
        return;
    }
    let (outbound, inbound) = gossip.drain_ready();
    for envelope in outbound {
        publish(
            gossip.readiness.topic(),
            net.as_deref(),
            &envelope,
            &mut commands,
        );
    }
    for envelope in inbound {
        commands.trigger(GossipReceived(envelope));
    }
}

fn broadcast(
    event: On<BroadcastGossip>,
    peers: Res<PeerIdNegotiationState>,
    net: Option<Res<NetworkResource>>,
    mut gossip: ResMut<GossipState>,
    mut commands: Commands,
) {
    if peers
        .assignment()
        .is_none_or(|seats| seats.local != event.0.player_id)
    {
        return;
    }
    match gossip.outbound(event.0.clone()) {
        Ok(Some(envelope)) => publish(
            gossip.readiness.topic(),
            net.as_deref(),
            &envelope,
            &mut commands,
        ),
        Ok(None) => {}
        Err(error) => commands.trigger(error),
    }
}

fn publish(
    topic: Option<&GossipTopic>,
    net: Option<&NetworkResource>,
    envelope: &GossipEnvelope,
    commands: &mut Commands,
) {
    let (Some(topic), Some(net)) = (topic, net) else {
        return;
    };
    if let Ok(payload) = rkyv::to_bytes::<rkyv::rancor::Error>(envelope)
        && let Err(error) = net.send(NetworkCommand::PublishGossip {
            topic: topic.clone(),
            payload: payload.to_vec(),
        })
    {
        commands.trigger(ConnectionLostEvent(DisconnectReason::Error(
            error.to_string(),
        )));
    }
}

fn receive(
    event: On<RawGossipMessageEvent>,
    peers: Res<PeerIdNegotiationState>,
    mut gossip: ResMut<GossipState>,
    mut commands: Commands,
) {
    let (Some(source), Some(seats)) = (event.source, peers.assignment()) else {
        return;
    };
    if gossip.readiness.topic() != Some(&event.topic)
        || source.to_bytes() != seats.remote.as_slice()
        || event.payload.len() > thespan::GAMEPLAY_MESSAGE_MAX_BYTES
    {
        return;
    }
    let mut bytes = rkyv::util::AlignedVec::<16>::with_capacity(event.payload.len());
    bytes.extend_from_slice(&event.payload);
    if let Ok(envelope) = rkyv::from_bytes::<GossipEnvelope, rkyv::rancor::Error>(&bytes)
        && envelope.player_id == seats.local.other()
        && envelope.seq.raw() != 0
        && let Some(envelope) = gossip.inbound(envelope)
    {
        commands.trigger(GossipReceived(envelope));
    }
}
