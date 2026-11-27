//! Checkpoint lists that omit reconstructible predecessor hashes.

use thiserror::Error;

use crate::commitment::{ChainError, CommitmentChain, StateCommitment};

/// A commitment with a replay tick in place of an explicit predecessor hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct CompactCheckpoint {
    pub sequence: u64,
    pub tick: u32,
    pub state_hash: [u8; 32],
    pub action_hash: [u8; 32],
    pub commitment_hash: [u8; 32],
}

/// Restore the explicit predecessor hash without recomputing the supplied commitment.
pub fn to_state_commitment(
    checkpoint: &CompactCheckpoint,
    prev_commitment: [u8; 32],
) -> StateCommitment {
    StateCommitment {
        sequence: checkpoint.sequence,
        state_hash: checkpoint.state_hash,
        action_hash: checkpoint.action_hash,
        prev_commitment,
        commitment_hash: checkpoint.commitment_hash,
    }
}

/// Omit the predecessor hash and associate the commitment with a replay tick.
pub fn from_state_commitment(commitment: &StateCommitment, tick: u32) -> CompactCheckpoint {
    CompactCheckpoint {
        sequence: commitment.sequence,
        tick,
        state_hash: commitment.state_hash,
        action_hash: commitment.action_hash,
        commitment_hash: commitment.commitment_hash,
    }
}

/// The checkpoint layout or reconstructed commitment chain was invalid.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompactChainError {
    #[error(transparent)]
    Chain(#[from] ChainError),
    #[error("genesis must be at tick zero and have no action")]
    InvalidGenesisLayout,
    #[error("checkpoint {sequence} does not advance the tick")]
    NonIncreasingTick { sequence: u64 },
}

/// Verify the layout and reconstruct and verify the commitment chain.
///
/// The genesis tick must be zero and later ticks strictly increase. Ticks are
/// not included in the commitment hashes. Callers must independently verify
/// replay timing and bind the chain to a trusted state or commitment hash.
pub fn verify_compact_checkpoints(
    checkpoints: &[CompactCheckpoint],
) -> Result<(), CompactChainError> {
    let first = checkpoints.first().ok_or(ChainError::EmptyChain)?;
    if first.tick != 0 || first.action_hash != [0; 32] {
        return Err(CompactChainError::InvalidGenesisLayout);
    }

    let mut chain = CommitmentChain {
        genesis: first.state_hash,
        commitments: Vec::with_capacity(checkpoints.len()),
    };
    let mut previous_hash = [0; 32];
    let mut previous_tick = None;
    for checkpoint in checkpoints {
        if previous_tick.is_some_and(|tick| tick >= checkpoint.tick) {
            return Err(CompactChainError::NonIncreasingTick {
                sequence: checkpoint.sequence,
            });
        }
        chain
            .commitments
            .push(to_state_commitment(checkpoint, previous_hash));
        previous_hash = checkpoint.commitment_hash;
        previous_tick = Some(checkpoint.tick);
    }
    Ok(chain.verify_integrity()?)
}
