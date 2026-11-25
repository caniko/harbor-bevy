//! Equal-peer recovery. Only the application's snapshot codec applies state.

use std::{marker::PhantomData, time::Duration};

use bevy::{prelude::*, time::Real};

use crate::{
    BattleNumber, ChainHash, ConnectionLifecyclePlugin, ConnectionLostEvent, ConnectionPhase,
    DisconnectReason, FromNetwork, GameNet, LocalEpoch, NetConfig, PeerIdNegotiationState,
    PeerSlotsAssigned, RoundNumber, SendControl, SessionCleanupRequested, SessionEpoch,
    SessionFault, SnapshotCodec, TransportHealth, TurnNumber, peer_id::admitted_peer,
};

pub const STATE_SYNC_ENCODED_LIMIT: usize = thespan::STATE_SYNC_MESSAGE_MAX_BYTES;
pub const DEFAULT_REJOIN_WINDOW: Duration = Duration::from_secs(60);
pub type SnapshotProgress = (BattleNumber, RoundNumber, TurnNumber);

#[derive(Debug, Resource)]
pub struct ReconnectionState {
    pub started_at: Duration,
    pub timeout: Duration,
    pub sync_received: bool,
    pub await_ack: bool,
    pub expected_ack_sequence: Option<u64>,
    pub telemetry_started: bool,
}

impl ReconnectionState {
    pub fn new(started_at: Duration, config: &NetConfig) -> Self {
        Self {
            started_at,
            timeout: Duration::from_secs(config.rejoin_window_secs),
            sync_received: false,
            await_ack: false,
            expected_ack_sequence: None,
            telemetry_started: false,
        }
    }

    pub fn expired(&self, now: Duration) -> bool {
        now.saturating_sub(self.started_at) >= self.timeout
    }
}

impl Default for ReconnectionState {
    fn default() -> Self {
        Self::new(Duration::ZERO, &NetConfig::default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejoinStep {
    RejoinRequest,
    RejoinAccepted,
    RequestStateSync,
    GameStateSync,
    StateSyncAck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Event, thiserror::Error)]
#[error("Recovery step {step:?} timed out after {attempts} attempts")]
pub struct RejoinStepTimeout {
    pub step: RejoinStep,
    pub attempts: u8,
}

#[derive(Debug, Clone, Copy, Resource)]
pub struct RejoinRetryState {
    pub step: RejoinStep,
    pub attempts: u8,
    pub started_at: Duration,
    pub next_retry_at: Duration,
}

impl RejoinRetryState {
    const DELAYS: [Duration; 3] = [
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(4),
    ];

    pub fn new(step: RejoinStep, now: Duration) -> Self {
        Self {
            step,
            attempts: 1,
            started_at: now,
            next_retry_at: now + Self::DELAYS[0],
        }
    }

    /// `true` means retransmit; the initial send is counted as attempt one.
    pub fn poll(&mut self, now: Duration) -> Result<bool, RejoinStepTimeout> {
        if now < self.next_retry_at {
            return Ok(false);
        }
        let Some(delay) = Self::DELAYS.get(usize::from(self.attempts)) else {
            return Err(RejoinStepTimeout {
                step: self.step,
                attempts: self.attempts,
            });
        };
        self.attempts += 1;
        self.next_retry_at = now + *delay;
        Ok(true)
    }
}

#[derive(Debug, Default, Resource)]
pub struct StateMismatchRecovery {
    pub attempted: bool,
    pub pending_hash_verification: bool,
    pub expected: Option<SnapshotProgress>,
}

impl StateMismatchRecovery {
    pub fn mismatch(&mut self, expected: SnapshotProgress) -> Result<(), SessionFault> {
        if self.attempted {
            return Err(SessionFault::Desync);
        }
        self.attempted = true;
        self.pending_hash_verification = true;
        self.expected = Some(expected);
        Ok(())
    }

    pub fn verified(&mut self, progress: SnapshotProgress) {
        if self.expected.is_some_and(|expected| progress >= expected) {
            self.pending_hash_verification = false;
        }
    }
}

#[derive(Debug, Default, Resource)]
pub struct StateSyncTracker {
    sequence: u64,
    state_sync_sequence: u64,
    last_applied_sync_sequence: u64,
    pending_sync: Option<GameStateSync>,
}

impl StateSyncTracker {
    pub fn last_applied_sync_sequence(&self) -> u64 {
        self.last_applied_sync_sequence
    }

    pub fn pending_sync(&self) -> Option<&GameStateSync> {
        self.pending_sync.as_ref()
    }

    pub fn apply<S: SnapshotCodec>(
        &mut self,
        sync: &GameStateSync,
        match_id: [u8; 16],
        minimum: Option<SnapshotProgress>,
        world: &mut World,
    ) -> Result<(), StateSyncError> {
        if sync.sequence <= self.last_applied_sync_sequence {
            return Err(StateSyncError::Stale);
        }
        if sync.epoch.match_id != match_id {
            return Err(StateSyncError::WrongMatch);
        }
        sync.check_size()?;
        let snapshot = S::decode(&sync.snapshot_bytes).map_err(codec_error)?;
        let (battle, round, turn) = snapshot.progress();
        if (battle, round) != (sync.epoch.battle_number, sync.epoch.round_number) {
            return Err(StateSyncError::ProgressMismatch);
        }
        let progress = (battle, round, turn);
        let local = S::capture(world).map_err(codec_error)?.progress();
        if progress < local || minimum.is_some_and(|expected| progress < expected) {
            return Err(StateSyncError::Rewind);
        }
        if snapshot.chain_hash() != sync.chain_hash {
            return Err(StateSyncError::HashMismatch);
        }
        snapshot.apply(world).map_err(codec_error)?;
        self.last_applied_sync_sequence = sync.sequence;
        Ok(())
    }
}

#[derive(Debug, Default, Resource)]
pub struct StateSyncSnapshotApplyBarrier {
    frames_remaining: u8,
}

impl StateSyncSnapshotApplyBarrier {
    pub fn start(&mut self) {
        self.frames_remaining = 2;
    }

    pub fn is_active(&self) -> bool {
        self.frames_remaining != 0
    }

    pub fn tick(&mut self) {
        self.frames_remaining = self.frames_remaining.saturating_sub(1);
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct RejoinRequest {
    pub match_id: [u8; 16],
    pub nonce: [u8; 32],
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct RejoinAccepted {
    pub epoch: SessionEpoch,
    pub nonce: [u8; 32],
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub enum RejoinDenied {
    WindowExpired,
    MatchMismatch,
    NonceMismatch,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct RequestStateSync {
    pub epoch: SessionEpoch,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct GameStateSync {
    pub sequence: u64,
    pub epoch: SessionEpoch,
    pub snapshot_bytes: Vec<u8>,
    pub chain_hash: ChainHash,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct StateSyncAck {
    pub sequence: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum StateSyncError {
    #[error("Encoded snapshot exceeds {STATE_SYNC_ENCODED_LIMIT} bytes")]
    Oversized,
    #[error("Snapshot belongs to a different match")]
    WrongMatch,
    #[error("Snapshot epoch does not match its contents")]
    ProgressMismatch,
    #[error("Snapshot would rewind local progress")]
    Rewind,
    #[error("Snapshot sequence is not newer")]
    Stale,
    #[error("Snapshot chain hash does not match its contents")]
    HashMismatch,
    #[error("Snapshot codec error: {0}")]
    Codec(String),
}

impl GameStateSync {
    pub fn capture<S: SnapshotCodec>(
        world: &World,
        sequence: u64,
        epoch: SessionEpoch,
    ) -> Result<Self, StateSyncError> {
        let snapshot = S::capture(world).map_err(codec_error)?;
        let (battle, round, _) = snapshot.progress();
        if (battle, round) != (epoch.battle_number, epoch.round_number) {
            return Err(StateSyncError::ProgressMismatch);
        }
        let sync = Self {
            sequence,
            epoch,
            chain_hash: snapshot.chain_hash(),
            snapshot_bytes: snapshot.encode().map_err(codec_error)?,
        };
        sync.check_size()?;
        Ok(sync)
    }

    fn archive(&self) -> Result<rkyv::util::AlignedVec, StateSyncError> {
        if self.snapshot_bytes.len() >= STATE_SYNC_ENCODED_LIMIT {
            return Err(StateSyncError::Oversized);
        }
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(self).map_err(codec_error)?;
        if bytes.len() > STATE_SYNC_ENCODED_LIMIT {
            return Err(StateSyncError::Oversized);
        }
        Ok(bytes)
    }

    fn check_size(&self) -> Result<(), StateSyncError> {
        self.archive().map(|_| ())
    }

    pub fn encode(&self) -> Result<Vec<u8>, StateSyncError> {
        self.archive().map(|bytes| bytes.to_vec())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, StateSyncError> {
        if bytes.len() > STATE_SYNC_ENCODED_LIMIT {
            return Err(StateSyncError::Oversized);
        }
        let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(bytes.len());
        aligned.extend_from_slice(bytes);
        rkyv::from_bytes::<Self, rkyv::rancor::Error>(&aligned).map_err(codec_error)
    }
}

fn codec_error(error: impl std::fmt::Display) -> StateSyncError {
    StateSyncError::Codec(error.to_string())
}

/// Trigger once the replacement transport is ready for control messages.
#[derive(Debug, Event)]
pub struct BeginRejoin;

#[derive(Debug, Event)]
pub struct StateHashMismatch(pub SnapshotProgress);

#[derive(Debug, Event)]
pub struct StateHashVerified(pub SnapshotProgress);

#[derive(Debug, Event)]
pub struct StateSyncSettled;

#[derive(Debug, Clone, Copy)]
enum Request {
    Rejoin(RejoinRequest),
    Snapshot(RequestStateSync),
}

#[derive(Default, Resource)]
struct Exchanges {
    request: Option<(RejoinRetryState, Request)>,
    response: Option<RejoinRetryState>,
}

pub struct StateSyncPlugin<G: GameNet>(PhantomData<fn() -> G>);

impl<G: GameNet> Default for StateSyncPlugin<G> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<G: GameNet> Plugin for StateSyncPlugin<G> {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<ConnectionLifecyclePlugin>() {
            app.add_plugins(ConnectionLifecyclePlugin);
        }
        app.init_resource::<NetConfig>()
            .init_resource::<PeerIdNegotiationState>()
            .init_resource::<LocalEpoch>()
            .init_resource::<StateSyncTracker>()
            .init_resource::<StateMismatchRecovery>()
            .init_resource::<StateSyncSnapshotApplyBarrier>()
            .init_resource::<Exchanges>()
            .add_systems(OnEnter(TransportHealth::Reconnecting), open_window)
            .add_systems(OnEnter(TransportHealth::Lost), open_window)
            .add_systems(First, settle)
            .add_systems(Update, retry)
            .add_observer(
                |event: On<PeerSlotsAssigned>, mut epoch: ResMut<LocalEpoch>| {
                    if epoch.0.match_id != event.match_id {
                        epoch.0 = SessionEpoch {
                            match_id: event.match_id,
                            ..SessionEpoch::SENTINEL
                        };
                    }
                },
            )
            .add_observer(begin)
            .add_observer(rejoin_request)
            .add_observer(rejoin_accepted)
            .add_observer(rejoin_denied)
            .add_observer(capture::<G::Snapshot>)
            .add_observer(apply::<G::Snapshot>)
            .add_observer(sync_ack)
            .add_observer(mismatch)
            .add_observer(
                |event: On<StateHashVerified>,
                 barrier: Res<StateSyncSnapshotApplyBarrier>,
                 mut recovery: ResMut<StateMismatchRecovery>| {
                    if !barrier.is_active() {
                        recovery.verified(event.0);
                    }
                },
            )
            .add_observer(reset);
    }
}

fn open_window(
    existing: Option<Res<ReconnectionState>>,
    config: Res<NetConfig>,
    time: Res<Time<Real>>,
    mut commands: Commands,
) {
    if existing.is_none() {
        commands.insert_resource(ReconnectionState::new(time.elapsed(), &config));
    }
}

fn begin(
    _: On<BeginRejoin>,
    phase: Res<State<ConnectionPhase>>,
    peers: Res<PeerIdNegotiationState>,
    time: Res<Time<Real>>,
    config: Res<NetConfig>,
    existing: Option<Res<ReconnectionState>>,
    mut health: ResMut<NextState<TransportHealth>>,
    mut exchange: ResMut<Exchanges>,
    mut commands: Commands,
) {
    let Some(seats) = peers.assignment().filter(|_| phase.get().is_in_session()) else {
        return;
    };
    if exchange.request.is_some() {
        return;
    }
    if existing
        .as_ref()
        .is_some_and(|state| state.expired(time.elapsed()))
    {
        commands.trigger(ConnectionLostEvent(DisconnectReason::Timeout));
        return;
    }
    if existing.is_none() {
        commands.insert_resource(ReconnectionState::new(time.elapsed(), &config));
    }
    health.set(TransportHealth::Rejoining);
    set_request(
        &mut exchange,
        seats,
        Request::Rejoin(RejoinRequest {
            match_id: seats.match_id,
            nonce: seats.local_nonce,
        }),
        time.elapsed(),
        &mut commands,
    );
}

fn rejoin_request(
    event: On<FromNetwork<RejoinRequest>>,
    peers: Res<PeerIdNegotiationState>,
    recovery: Option<Res<ReconnectionState>>,
    time: Res<Time<Real>>,
    epoch: Res<LocalEpoch>,
    mut health: ResMut<NextState<TransportHealth>>,
    mut commands: Commands,
) {
    let Some(seats) = admitted_peer(&peers, event.from.as_ref()) else {
        return;
    };
    let reason = if event.inner.match_id != seats.match_id || epoch.0.match_id != seats.match_id {
        RejoinDenied::MatchMismatch
    } else if event.inner.nonce != seats.remote_nonce {
        RejoinDenied::NonceMismatch
    } else if recovery.is_none_or(|state| state.expired(time.elapsed())) {
        RejoinDenied::WindowExpired
    } else {
        if matches!(*health, NextState::Pending(TransportHealth::Disconnecting)) {
            return;
        }
        health.set(TransportHealth::Rejoining);
        commands.trigger(SendControl::new(
            seats.remote.clone(),
            RejoinAccepted {
                epoch: epoch.0,
                nonce: event.inner.nonce,
            },
        ));
        return;
    };
    commands.trigger(SendControl::new(seats.remote.clone(), reason));
}

fn rejoin_accepted(
    event: On<FromNetwork<RejoinAccepted>>,
    peers: Res<PeerIdNegotiationState>,
    time: Res<Time<Real>>,
    epoch: Res<LocalEpoch>,
    mut tracker: ResMut<StateSyncTracker>,
    mut exchange: ResMut<Exchanges>,
    mut commands: Commands,
) {
    let Some(seats) = admitted_peer(&peers, event.from.as_ref()) else {
        return;
    };
    if event.inner.epoch.match_id != seats.match_id
        || epoch.0.match_id != seats.match_id
        || event.inner.nonce != seats.local_nonce
        || !matches!(exchange.request, Some((_, Request::Rejoin(_))))
    {
        return;
    }
    request_snapshot(
        &mut tracker,
        &mut exchange,
        seats,
        epoch.0,
        time.elapsed(),
        &mut commands,
    );
}

fn rejoin_denied(
    event: On<FromNetwork<RejoinDenied>>,
    peers: Res<PeerIdNegotiationState>,
    mut exchange: ResMut<Exchanges>,
    mut commands: Commands,
) {
    if admitted_peer(&peers, event.from.as_ref()).is_some()
        && matches!(exchange.request, Some((_, Request::Rejoin(_))))
    {
        exchange.request = None;
        let reason = match event.inner {
            RejoinDenied::WindowExpired => DisconnectReason::Timeout,
            denial => DisconnectReason::Error(format!("Rejoin denied: {denial:?}")),
        };
        commands.trigger(ConnectionLostEvent(reason));
    }
}

fn request_snapshot(
    tracker: &mut StateSyncTracker,
    exchange: &mut Exchanges,
    seats: &PeerSlotsAssigned,
    epoch: SessionEpoch,
    now: Duration,
    commands: &mut Commands,
) {
    let Some(sequence) = tracker.sequence.checked_add(1) else {
        commands.trigger(SessionFault::Desync);
        return;
    };
    tracker.sequence = sequence;
    set_request(
        exchange,
        seats,
        Request::Snapshot(RequestStateSync { epoch, sequence }),
        now,
        commands,
    );
}

fn set_request(
    exchange: &mut Exchanges,
    seats: &PeerSlotsAssigned,
    request: Request,
    now: Duration,
    commands: &mut Commands,
) {
    let step = match request {
        Request::Rejoin(_) => RejoinStep::RejoinRequest,
        Request::Snapshot(_) => RejoinStep::RequestStateSync,
    };
    send_request(seats, request, commands);
    exchange.request = Some((RejoinRetryState::new(step, now), request));
}

fn send_request(seats: &PeerSlotsAssigned, request: Request, commands: &mut Commands) {
    match request {
        Request::Rejoin(msg) => commands.trigger(SendControl::new(seats.remote.clone(), msg)),
        Request::Snapshot(msg) => commands.trigger(SendControl::new(seats.remote.clone(), msg)),
    }
}

fn mismatch(
    event: On<StateHashMismatch>,
    peers: Res<PeerIdNegotiationState>,
    epoch: Res<LocalEpoch>,
    time: Res<Time<Real>>,
    mut recovery: ResMut<StateMismatchRecovery>,
    mut tracker: ResMut<StateSyncTracker>,
    mut exchange: ResMut<Exchanges>,
    mut commands: Commands,
) {
    let Some(seats) = peers
        .assignment()
        .filter(|seats| seats.match_id == epoch.0.match_id)
    else {
        return;
    };
    if let Err(fault) = recovery.mismatch(event.0) {
        commands.trigger(fault);
    } else if exchange.request.is_none() {
        request_snapshot(
            &mut tracker,
            &mut exchange,
            seats,
            epoch.0,
            time.elapsed(),
            &mut commands,
        );
    }
}

fn capture<S: SnapshotCodec>(
    event: On<FromNetwork<RequestStateSync>>,
    peers: Res<PeerIdNegotiationState>,
    mut commands: Commands,
) {
    let Some(seats) = admitted_peer(&peers, event.from.as_ref()) else {
        return;
    };
    if event.inner.epoch.match_id != seats.match_id || event.inner.sequence == 0 {
        return;
    }
    let sequence = event.inner.sequence;
    let recipient = seats.remote.clone();
    let match_id = seats.match_id;
    commands.queue(move |world: &mut World| {
        let epoch = world.resource::<LocalEpoch>().0;
        if epoch.match_id != match_id {
            return;
        }
        let tracker = world.resource::<StateSyncTracker>();
        if let Some(pending) = tracker
            .pending_sync()
            .filter(|sync| sync.sequence == sequence)
        {
            world.trigger(SendControl::new(recipient, pending.clone()));
            return;
        }
        if sequence <= tracker.state_sync_sequence {
            return;
        }
        let sync = match GameStateSync::capture::<S>(world, sequence, epoch) {
            Ok(sync) => sync,
            Err(error) => {
                tracing::warn!(%error, "state sync capture rejected");
                world.trigger(SessionFault::Desync);
                return;
            }
        };
        let mut tracker = world.resource_mut::<StateSyncTracker>();
        tracker.state_sync_sequence = sequence;
        tracker.pending_sync = Some(sync.clone());
        if let Some(mut state) = world.get_resource_mut::<ReconnectionState>() {
            state.await_ack = true;
            state.expected_ack_sequence = Some(sequence);
        }
        let now = world.resource::<Time<Real>>().elapsed();
        world.resource_mut::<Exchanges>().response =
            Some(RejoinRetryState::new(RejoinStep::GameStateSync, now));
        world.trigger(SendControl::new(recipient, sync));
    });
}

fn apply<S: SnapshotCodec>(
    event: On<FromNetwork<GameStateSync>>,
    peers: Res<PeerIdNegotiationState>,
    tracker: Res<StateSyncTracker>,
    exchange: Res<Exchanges>,
    mut commands: Commands,
) {
    let Some(seats) = admitted_peer(&peers, event.from.as_ref()) else {
        return;
    };
    let (sequence, match_id) = (event.inner.sequence, seats.match_id);
    if event.inner.epoch.match_id != match_id || sequence == 0 {
        return;
    }
    let recipient = seats.remote.clone();
    if sequence == tracker.last_applied_sync_sequence {
        commands.trigger(SendControl::new(recipient, StateSyncAck { sequence }));
        return;
    }
    if !matches!(exchange.request, Some((_, Request::Snapshot(req))) if req.sequence == sequence) {
        return;
    }
    if event.inner.snapshot_bytes.len() >= STATE_SYNC_ENCODED_LIMIT {
        commands.trigger(SessionFault::Desync);
        return;
    }
    let sync = event.inner.clone();
    commands.queue(move |world: &mut World| {
        let minimum = world.resource::<StateMismatchRecovery>().expected;
        let result = world.resource_scope(|world, mut tracker: Mut<StateSyncTracker>| {
            tracker.apply::<S>(&sync, match_id, minimum, world)
        });
        if let Err(error) = result {
            if !matches!(error, StateSyncError::Stale) {
                tracing::warn!(%error, "state sync apply rejected");
                world.trigger(SessionFault::Desync);
            }
            return;
        }
        world.resource_mut::<LocalEpoch>().0 = sync.epoch;
        world.resource_mut::<Exchanges>().request = None;
        if let Some(mut state) = world.get_resource_mut::<ReconnectionState>() {
            state.sync_received = true;
        }
        world
            .resource_mut::<StateSyncSnapshotApplyBarrier>()
            .start();
        world.trigger(SendControl::new(recipient, StateSyncAck { sequence }));
    });
}

fn sync_ack(
    event: On<FromNetwork<StateSyncAck>>,
    peers: Res<PeerIdNegotiationState>,
    mut tracker: ResMut<StateSyncTracker>,
    mut exchange: ResMut<Exchanges>,
    barrier: Res<StateSyncSnapshotApplyBarrier>,
    state: Option<ResMut<ReconnectionState>>,
    mut health: ResMut<NextState<TransportHealth>>,
    mut commands: Commands,
) {
    if admitted_peer(&peers, event.from.as_ref()).is_none()
        || tracker
            .pending_sync()
            .is_none_or(|sync| sync.sequence != event.inner.sequence)
    {
        return;
    }
    tracker.pending_sync = None;
    exchange.response = None;
    if let Some(mut state) = state {
        state.await_ack = false;
        state.expected_ack_sequence = None;
        if exchange.request.is_none() && !barrier.is_active() {
            finish_rejoin(&mut health, &mut commands);
        }
    }
}

fn settle(
    mut barrier: ResMut<StateSyncSnapshotApplyBarrier>,
    exchange: Res<Exchanges>,
    recovery: Option<Res<ReconnectionState>>,
    mut health: ResMut<NextState<TransportHealth>>,
    mut commands: Commands,
) {
    if !barrier.is_active() {
        return;
    }
    barrier.tick();
    if !barrier.is_active() {
        if recovery.is_some() && exchange.response.is_none() && exchange.request.is_none() {
            finish_rejoin(&mut health, &mut commands);
        } else if recovery.is_none() {
            commands.trigger(StateSyncSettled);
        }
    }
}

fn finish_rejoin(health: &mut NextState<TransportHealth>, commands: &mut Commands) {
    if !matches!(*health, NextState::Pending(TransportHealth::Disconnecting)) {
        health.set(TransportHealth::Healthy);
        commands.remove_resource::<ReconnectionState>();
        commands.trigger(StateSyncSettled);
    }
}

fn retry(
    time: Res<Time<Real>>,
    peers: Res<PeerIdNegotiationState>,
    reconnection: Option<Res<ReconnectionState>>,
    recovery: Res<StateMismatchRecovery>,
    tracker: Res<StateSyncTracker>,
    mut exchange: ResMut<Exchanges>,
    mut commands: Commands,
) {
    let Some(seats) = peers.assignment() else {
        return;
    };
    let now = time.elapsed();
    if reconnection.is_some_and(|state| state.expired(now)) {
        commands.remove_resource::<ReconnectionState>();
        commands.trigger(ConnectionLostEvent(DisconnectReason::Timeout));
        return;
    }
    if let Some((retry, request)) = &mut exchange.request {
        match retry.poll(now) {
            Ok(true) => send_request(seats, *request, &mut commands),
            Ok(false) => {}
            Err(error) => {
                exchange.request = None;
                commands.trigger(error);
                if recovery.pending_hash_verification {
                    commands.trigger(SessionFault::Desync);
                } else {
                    commands.trigger(ConnectionLostEvent(DisconnectReason::Timeout));
                }
                return;
            }
        }
    }
    if let Some(retry) = &mut exchange.response {
        match retry.poll(now) {
            Ok(true) => {
                if let Some(sync) = tracker.pending_sync() {
                    commands.trigger(SendControl::new(seats.remote.clone(), sync.clone()));
                }
            }
            Ok(false) => {}
            Err(error) => {
                exchange.response = None;
                commands.trigger(error);
                commands.trigger(ConnectionLostEvent(DisconnectReason::Timeout));
            }
        }
    }
}

fn reset(
    _: On<SessionCleanupRequested>,
    mut tracker: ResMut<StateSyncTracker>,
    mut mismatch: ResMut<StateMismatchRecovery>,
    mut barrier: ResMut<StateSyncSnapshotApplyBarrier>,
    mut exchange: ResMut<Exchanges>,
    mut epoch: ResMut<LocalEpoch>,
    mut commands: Commands,
) {
    *tracker = StateSyncTracker::default();
    *mismatch = StateMismatchRecovery::default();
    *barrier = StateSyncSnapshotApplyBarrier::default();
    *exchange = Exchanges::default();
    *epoch = LocalEpoch::default();
    commands.remove_resource::<ReconnectionState>();
}
