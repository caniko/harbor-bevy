use bevy::prelude::*;
use sha2::{Digest as _, Sha256};
use thespan_bevy::FromNetwork;
use veritasium_wire::{PeerIdBytes, PlayerId};

use crate::{
    ConnectionLifecyclePlugin, ConnectionPhase, SessionCleanupRequested, SessionFault,
    SharedRngSeed,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum NegotiationPhase {
    #[default]
    Idle,
    WaitingForTheirCommit,
    WaitingForOurReveal,
    WaitingForTheirReveal,
    Complete,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct PeerIdCommit {
    pub hash: [u8; 32],
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct PeerIdReveal {
    pub nonce: [u8; 32],
}

impl PeerIdReveal {
    fn commit(self) -> PeerIdCommit {
        PeerIdCommit {
            hash: Sha256::digest(self.nonce).into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PeerIdCommitError {
    #[error("A different peer commitment was already recorded")]
    ConflictingCommit,
    #[error("Peer negotiation is not active")]
    NotNegotiating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PeerIdRevealError {
    #[error("Both participants must commit before revealing")]
    NotReady,
    #[error("Peer reveal did not match its commitment")]
    CommitmentMismatch,
    #[error("Participants revealed identical nonces")]
    EqualNonces,
    #[error("Saved assignment is invalid or conflicts with the current negotiation")]
    ConflictingAssignment,
}

#[derive(Debug, Clone, PartialEq, Eq, Event)]
pub struct PeerSlotsAssigned {
    pub local: PlayerId,
    pub remote: PeerIdBytes,
    pub local_nonce: [u8; 32],
    pub remote_nonce: [u8; 32],
    pub match_id: [u8; 16],
}

impl PeerSlotsAssigned {
    pub fn rng_seed(&self) -> SharedRngSeed {
        SharedRngSeed::Session(crate::rng::nonce_seed(self.local_nonce, self.remote_nonce))
    }
}

#[derive(Default, Resource)]
pub struct PeerIdNegotiationState {
    phase: NegotiationPhase,
    remote: Option<PeerIdBytes>,
    local_reveal: Option<PeerIdReveal>,
    remote_commit: Option<PeerIdCommit>,
    assignment: Option<PeerSlotsAssigned>,
}

impl PeerIdNegotiationState {
    pub fn phase(&self) -> NegotiationPhase {
        self.phase
    }

    pub fn assignment(&self) -> Option<&PeerSlotsAssigned> {
        self.assignment.as_ref()
    }

    pub fn begin(&mut self, remote: PeerIdBytes, nonce: [u8; 32]) -> Option<PeerIdCommit> {
        if self.phase != NegotiationPhase::Idle || remote.is_empty() {
            return None;
        }
        let reveal = PeerIdReveal { nonce };
        self.remote = Some(remote);
        self.local_reveal = Some(reveal);
        self.phase = NegotiationPhase::WaitingForTheirCommit;
        Some(reveal.commit())
    }

    pub fn receive_commit(&mut self, commit: PeerIdCommit) -> Result<(), PeerIdCommitError> {
        if let Some(first) = self.remote_commit {
            return if first == commit {
                Ok(())
            } else {
                Err(PeerIdCommitError::ConflictingCommit)
            };
        }
        if self.phase != NegotiationPhase::WaitingForTheirCommit {
            return Err(PeerIdCommitError::NotNegotiating);
        }
        self.remote_commit = Some(commit);
        self.phase = NegotiationPhase::WaitingForOurReveal;
        Ok(())
    }

    pub fn reveal(&mut self) -> Option<PeerIdReveal> {
        match self.phase {
            NegotiationPhase::WaitingForOurReveal => {
                self.phase = NegotiationPhase::WaitingForTheirReveal;
                self.local_reveal
            }
            NegotiationPhase::WaitingForTheirReveal | NegotiationPhase::Complete => {
                self.local_reveal
            }
            _ => None,
        }
    }

    pub fn receive_reveal(
        &mut self,
        reveal: PeerIdReveal,
    ) -> Result<Option<PeerSlotsAssigned>, PeerIdRevealError> {
        if let Some(assigned) = &self.assignment {
            return if assigned.remote_nonce == reveal.nonce {
                Ok(None)
            } else {
                Err(PeerIdRevealError::CommitmentMismatch)
            };
        }
        let (Some(local), Some(remote)) = (self.local_reveal, &self.remote) else {
            return Err(PeerIdRevealError::NotReady);
        };
        if self.phase != NegotiationPhase::WaitingForTheirReveal {
            return Err(PeerIdRevealError::NotReady);
        }
        if Some(reveal.commit()) != self.remote_commit {
            return Err(PeerIdRevealError::CommitmentMismatch);
        }
        if local == reveal {
            return Err(PeerIdRevealError::EqualNonces);
        }
        let assigned = PeerSlotsAssigned {
            local: if local.nonce < reveal.nonce {
                PlayerId::ZERO
            } else {
                PlayerId::ONE
            },
            remote: remote.clone(),
            local_nonce: local.nonce,
            remote_nonce: reveal.nonce,
            match_id: match_id(local.nonce, reveal.nonce),
        };
        self.phase = NegotiationPhase::Complete;
        self.assignment = Some(assigned.clone());
        Ok(Some(assigned))
    }

    /// Restore from trusted saved state. The saved seat is authoritative for
    /// this restore; never run negotiation again to choose a new one.
    pub fn restore_for_rejoin(
        &mut self,
        saved: PeerSlotsAssigned,
    ) -> Result<(), PeerIdRevealError> {
        if self.assignment.as_ref() == Some(&saved) {
            return Ok(());
        }
        if self.phase != NegotiationPhase::Idle
            || saved.local.index() > 1
            || saved.remote.is_empty()
            || saved.local_nonce == saved.remote_nonce
            || saved.match_id != match_id(saved.local_nonce, saved.remote_nonce)
        {
            return Err(PeerIdRevealError::ConflictingAssignment);
        }
        self.phase = NegotiationPhase::Complete;
        self.remote = Some(saved.remote.clone());
        self.local_reveal = Some(PeerIdReveal {
            nonce: saved.local_nonce,
        });
        self.remote_commit = Some(
            PeerIdReveal {
                nonce: saved.remote_nonce,
            }
            .commit(),
        );
        self.assignment = Some(saved);
        Ok(())
    }
}

pub(crate) fn admitted_peer<'a>(
    negotiation: &'a PeerIdNegotiationState,
    from: Option<&PeerIdBytes>,
) -> Option<&'a PeerSlotsAssigned> {
    negotiation
        .assignment()
        .filter(|seats| Some(&seats.remote) == from)
}

fn match_id(a: [u8; 32], b: [u8; 32]) -> [u8; 16] {
    let [first, second] = if a < b { [a, b] } else { [b, a] };
    let mut hash = Sha256::new();
    hash.update(b"harbor-bevy-net/match-id/v1");
    hash.update(first);
    hash.update(second);
    let digest = hash.finalize();
    std::array::from_fn(|index| digest[index])
}

/// Start two-participant negotiation after admission. The application routes
/// the outbound events to the indicated peer using its session protocol.
#[derive(Debug, Event)]
pub struct BeginPeerNegotiation(pub PeerIdBytes);

#[derive(Debug, Event)]
pub struct PeerCommitReady(pub PeerIdBytes, pub PeerIdCommit);

#[derive(Debug, Event)]
pub struct PeerRevealReady(pub PeerIdBytes, pub PeerIdReveal);

pub struct PeerNegotiationPlugin;

impl Plugin for PeerNegotiationPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<ConnectionLifecyclePlugin>() {
            app.add_plugins(ConnectionLifecyclePlugin);
        }
        app.init_resource::<PeerIdNegotiationState>()
            .add_observer(begin)
            .add_observer(receive_commit)
            .add_observer(receive_reveal)
            .add_observer(
                |_: On<SessionCleanupRequested>, mut state: ResMut<PeerIdNegotiationState>| {
                    *state = PeerIdNegotiationState::default();
                },
            );
    }
}

fn begin(
    event: On<BeginPeerNegotiation>,
    phase: Res<State<ConnectionPhase>>,
    mut state: ResMut<PeerIdNegotiationState>,
    mut commands: Commands,
) {
    if *phase.get() == ConnectionPhase::Negotiating
        && let Some(commit) = state.begin(event.0.clone(), rand::random())
    {
        commands.trigger(PeerCommitReady(event.0.clone(), commit));
    }
}

fn receive_commit(
    event: On<FromNetwork<PeerIdCommit>>,
    mut state: ResMut<PeerIdNegotiationState>,
    mut commands: Commands,
) {
    if event.from.is_none() || event.from != state.remote {
        return;
    }
    match state.receive_commit(event.inner) {
        Ok(()) if state.phase() == NegotiationPhase::WaitingForOurReveal => {
            if let Some(reveal) = state.reveal() {
                commands.trigger(PeerRevealReady(
                    event.from.clone().expect("checked sender"),
                    reveal,
                ));
            }
        }
        Ok(()) => {}
        Err(error) => commands.trigger(SessionFault::ValidationViolation(error.to_string())),
    }
}

fn receive_reveal(
    event: On<FromNetwork<PeerIdReveal>>,
    mut state: ResMut<PeerIdNegotiationState>,
    mut phase: ResMut<NextState<ConnectionPhase>>,
    mut commands: Commands,
) {
    if event.from.is_none() || event.from != state.remote {
        return;
    }
    match state.receive_reveal(event.inner) {
        Ok(Some(assigned)) => {
            phase.set(ConnectionPhase::Connected);
            commands.trigger(assigned);
        }
        Ok(None) => {}
        Err(error) => commands.trigger(SessionFault::ValidationViolation(error.to_string())),
    }
}
