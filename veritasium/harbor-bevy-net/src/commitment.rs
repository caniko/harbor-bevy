use std::{marker::PhantomData, time::Duration};

use bevy::{prelude::*, time::Real};
use sha2::{Digest as _, Sha256};

use crate::{
    CommitPayload, FromNetwork, GameNet, NetConfig, PeerIdNegotiationState, PeerSlotsAssigned,
    SendControl, SessionCleanupRequested, SessionFault, peer_id::admitted_peer,
};

pub const COMMITMENT_ACK_RESEND_INTERVAL: Duration = Duration::from_secs(1);
pub const COMMITMENT_REVEAL_WAIT: Duration = Duration::from_secs(10);
pub const MAX_COMMITMENT_RETRIES: u8 = 3;
const MAX_COMMIT_PAYLOAD: usize = crate::MAX_SESSION_PACKET_BYTES - 128;

pub fn commitment_hash(domain: &str, turn: u32, payload: &[u8], nonce: &[u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"harbor-bevy-net/commitment/v1");
    hash.update((domain.len() as u64).to_le_bytes());
    hash.update(domain.as_bytes());
    hash.update(turn.to_le_bytes());
    hash.update(nonce);
    hash.update(payload);
    hash.finalize().into()
}

#[derive(Debug, Clone, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum CommitmentMessage {
    Commit {
        turn: u32,
        hash: [u8; 32],
    },
    Ack {
        turn: u32,
    },
    Reveal {
        turn: u32,
        payload: Vec<u8>,
        nonce: [u8; 32],
    },
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CommitmentPhase {
    #[default]
    Idle,
    WaitingForAck,
    ReadyToReveal,
    WaitingForReveal,
    Verified,
}

#[derive(Resource)]
pub struct CommitmentState<H: CommitPayload> {
    turn: Option<u32>,
    our_commitment: Option<[u8; 32]>,
    our_reveal: Option<CommitmentMessage>,
    peer_commitment: Option<[u8; 32]>,
    peer_acked: bool,
    sent_reveal: bool,
    verified: bool,
    retry_at: Duration,
    retry_count: u8,
    consecutive_failures: u32,
    faulted: bool,
    hidden: PhantomData<fn() -> H>,
}

impl<H: CommitPayload> Default for CommitmentState<H> {
    fn default() -> Self {
        Self {
            turn: None,
            our_commitment: None,
            our_reveal: None,
            peer_commitment: None,
            peer_acked: false,
            sent_reveal: false,
            verified: false,
            retry_at: Duration::ZERO,
            retry_count: 0,
            consecutive_failures: 0,
            faulted: false,
            hidden: PhantomData,
        }
    }
}

pub struct CommitmentReceipt<H> {
    pub reply: Option<CommitmentMessage>,
    pub verified: Option<H>,
    replay_reveal: bool,
}

impl<H: CommitPayload> CommitmentState<H> {
    pub fn phase(&self) -> CommitmentPhase {
        if self.verified {
            CommitmentPhase::Verified
        } else if self.our_commitment.is_none() {
            CommitmentPhase::Idle
        } else if !self.peer_acked || self.peer_commitment.is_none() {
            CommitmentPhase::WaitingForAck
        } else if self.sent_reveal {
            CommitmentPhase::WaitingForReveal
        } else {
            CommitmentPhase::ReadyToReveal
        }
    }

    pub fn peer_acked(&self) -> bool {
        self.peer_acked
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    pub fn reset_turn(&mut self, turn: u32) {
        if self.turn.is_none_or(|prior| turn > prior) {
            *self = Self {
                turn: Some(turn),
                consecutive_failures: self.consecutive_failures,
                ..Self::default()
            };
        }
    }

    pub fn begin(
        &mut self,
        value: &H,
        nonce: [u8; 32],
        domain: &str,
        now: Duration,
    ) -> Result<CommitmentMessage, SessionFault> {
        let Some(turn) = self
            .turn
            .filter(|_| self.our_commitment.is_none() && !self.faulted)
        else {
            return Err(SessionFault::ValidationViolation(
                "commitment is not ready to start".into(),
            ));
        };
        let payload = value
            .to_commit_bytes()
            .map_err(|e| SessionFault::ValidationViolation(e.to_string()))?;
        if payload.len() > MAX_COMMIT_PAYLOAD {
            return Err(SessionFault::ValidationViolation(
                "commitment payload exceeds the session limit".into(),
            ));
        }
        let hash = commitment_hash(domain, turn, &payload, &nonce);
        self.our_commitment = Some(hash);
        self.our_reveal = Some(CommitmentMessage::Reveal {
            turn,
            payload,
            nonce,
        });
        self.retry_at = now + COMMITMENT_ACK_RESEND_INTERVAL;
        Ok(CommitmentMessage::Commit { turn, hash })
    }

    pub fn receive(
        &mut self,
        message: &CommitmentMessage,
        domain: &str,
        now: Duration,
    ) -> Result<CommitmentReceipt<H>, SessionFault> {
        use CommitmentMessage::{Ack, Commit, Reveal};
        let mut receipt = CommitmentReceipt {
            reply: None,
            verified: None,
            replay_reveal: false,
        };
        let turn = match message {
            Commit { turn, .. } | Ack { turn } | Reveal { turn, .. } => *turn,
        };
        if self.turn != Some(turn) || self.faulted {
            return Ok(receipt);
        }
        match message {
            Commit { hash, .. } => {
                if self.peer_commitment.is_some_and(|prior| prior != *hash) {
                    return Err(self.fail(SessionFault::CommitmentMismatch));
                }
                receipt.replay_reveal = self.sent_reveal;
                self.peer_commitment = Some(*hash);
                receipt.reply = Some(Ack { turn });
            }
            Ack { .. } => {
                if !self.peer_acked && self.our_commitment.is_some() {
                    self.peer_acked = true;
                    self.retry_count = 0;
                    self.retry_at = now + COMMITMENT_ACK_RESEND_INTERVAL;
                }
            }
            Reveal { payload, nonce, .. } => {
                let Some(expected) = self
                    .peer_commitment
                    .filter(|_| self.our_commitment.is_some())
                else {
                    return Ok(receipt);
                };
                if payload.len() > MAX_COMMIT_PAYLOAD
                    || commitment_hash(domain, turn, payload, nonce) != expected
                {
                    return Err(self.fail(SessionFault::CommitmentMismatch));
                }
                if self.verified {
                    return Ok(receipt);
                }
                let hidden = H::from_commit_bytes(payload)
                    .map_err(|_| self.fail(SessionFault::CommitmentMismatch))?;
                self.verified = true;
                receipt.verified = Some(hidden);
                self.mark_complete();
            }
        }
        Ok(receipt)
    }

    pub fn take_reveal(&mut self, now: Duration) -> Option<CommitmentMessage> {
        if self.faulted || !self.peer_acked || self.peer_commitment.is_none() || self.sent_reveal {
            return None;
        }
        let reveal = self.our_reveal.clone()?;
        self.sent_reveal = true;
        self.retry_count = 0;
        self.retry_at = now + COMMITMENT_REVEAL_WAIT;
        self.mark_complete();
        Some(reveal)
    }

    pub fn tick(&mut self, now: Duration) -> Result<Option<CommitmentMessage>, SessionFault> {
        if self.faulted || now < self.retry_at {
            return Ok(None);
        }
        let (Some(turn), Some(hash)) = (self.turn, self.our_commitment) else {
            return Ok(None);
        };
        let interval = if !self.peer_acked || self.peer_commitment.is_none() {
            // Even Verified still owes a reveal when its commit ACK was lost.
            COMMITMENT_ACK_RESEND_INTERVAL
        } else if self.sent_reveal && !self.verified {
            COMMITMENT_REVEAL_WAIT
        } else {
            return Ok(None);
        };
        self.retry_count += 1;
        if self.retry_count > MAX_COMMITMENT_RETRIES {
            return Err(self.fail(SessionFault::CommitmentRetriesExhausted));
        }
        self.retry_at = now + interval;
        Ok(Some(CommitmentMessage::Commit { turn, hash }))
    }

    fn mark_complete(&mut self) {
        if self.verified && self.sent_reveal {
            self.consecutive_failures = 0;
        }
    }

    fn fail(&mut self, fault: SessionFault) -> SessionFault {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.faulted = true;
        fault
    }
}

#[derive(Debug, Event)]
pub struct PrepareCommitment(pub u32);

#[derive(Event)]
pub struct BeginCommitment<H: CommitPayload> {
    pub turn: u32,
    pub payload: H,
}

#[derive(Event)]
pub struct CommitmentVerified<H: CommitPayload> {
    pub turn: u32,
    pub payload: H,
}

pub struct CommitmentPlugin<G: GameNet>(PhantomData<fn() -> G>);

impl<G: GameNet> Default for CommitmentPlugin<G> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<G: GameNet> Plugin for CommitmentPlugin<G> {
    fn build(&self, app: &mut App) {
        app.init_resource::<NetConfig>()
            .init_resource::<PeerIdNegotiationState>()
            .init_resource::<CommitmentState<G::Hidden>>()
            .add_observer(
                |e: On<PrepareCommitment>, mut state: ResMut<CommitmentState<G::Hidden>>| {
                    state.reset_turn(e.0)
                },
            )
            .add_observer(
                |_: On<SessionCleanupRequested>, mut state: ResMut<CommitmentState<G::Hidden>>| {
                    *state = CommitmentState::default()
                },
            )
            .add_observer(begin::<G::Hidden>)
            .add_observer(receive::<G::Hidden>)
            .add_systems(Update, tick::<G::Hidden>);
    }
}

fn domain(config: &NetConfig, seats: &PeerSlotsAssigned, sender: crate::PlayerId) -> String {
    format!(
        "{}/commitment/{sender}",
        config.match_topic(&seats.match_id)
    )
}

fn begin<H: CommitPayload>(
    event: On<BeginCommitment<H>>,
    peers: Res<PeerIdNegotiationState>,
    config: Res<NetConfig>,
    time: Res<Time<Real>>,
    mut state: ResMut<CommitmentState<H>>,
    mut commands: Commands,
) {
    let Some(seats) = peers.assignment() else {
        return;
    };
    state.reset_turn(event.turn);
    if state.turn != Some(event.turn) || state.our_commitment.is_some() || state.faulted {
        return;
    }
    match state.begin(
        &event.payload,
        rand::random(),
        &domain(&config, seats, seats.local),
        time.elapsed(),
    ) {
        Ok(msg) => commands.trigger(SendControl::new(seats.remote.clone(), msg)),
        Err(fault) => commands.trigger(fault),
    }
}

fn receive<H: CommitPayload>(
    event: On<FromNetwork<CommitmentMessage>>,
    peers: Res<PeerIdNegotiationState>,
    config: Res<NetConfig>,
    time: Res<Time<Real>>,
    mut state: ResMut<CommitmentState<H>>,
    mut commands: Commands,
) {
    let Some(seats) = admitted_peer(&peers, event.from.as_ref()) else {
        return;
    };
    let now = time.elapsed();
    match state.receive(
        &event.inner,
        &domain(&config, seats, seats.local.other()),
        now,
    ) {
        Ok(receipt) => {
            if let Some(reply) = receipt.reply {
                commands.trigger(SendControl::new(seats.remote.clone(), reply));
            }
            if let Some(reveal) = state.take_reveal(now).or_else(|| {
                receipt
                    .replay_reveal
                    .then(|| state.our_reveal.clone())
                    .flatten()
            }) {
                commands.trigger(SendControl::new(seats.remote.clone(), reveal));
            }
            if let (Some(turn), Some(payload)) = (state.turn, receipt.verified) {
                commands.trigger(CommitmentVerified { turn, payload });
            }
        }
        Err(fault) => commands.trigger(fault),
    }
}

fn tick<H: CommitPayload>(
    peers: Res<PeerIdNegotiationState>,
    time: Res<Time<Real>>,
    mut state: ResMut<CommitmentState<H>>,
    mut commands: Commands,
) {
    let Some(seats) = peers.assignment() else {
        return;
    };
    match state.tick(time.elapsed()) {
        Ok(Some(message)) => commands.trigger(SendControl::new(seats.remote.clone(), message)),
        Ok(None) => {}
        Err(fault) => commands.trigger(fault),
    }
}
