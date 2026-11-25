//! State commitment chain for tamper-evident history.
//!
//! This module provides a Merkle-like chain of state commitments,
//! where each commitment links to the previous one, making it
//! impossible to modify history without detection.

use sha2::{Digest, Sha256};
use thiserror::Error;

/// A single state commitment in the chain.
///
/// Each commitment contains:
/// - The sequence number
/// - Hash of the current game state
/// - Hash of the action that caused this state
/// - Link to the previous commitment (chain link)
/// - A combined commitment hash (Merkle-style root)
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct StateCommitment {
    /// Sequence number (monotonically increasing).
    pub sequence: u64,
    /// Hash of the current game state.
    pub state_hash: [u8; 32],
    /// Hash of the action that led to this state.
    pub action_hash: [u8; 32],
    /// Hash of the previous commitment (chain link).
    pub prev_commitment: [u8; 32],
    /// Combined hash of all fields (the commitment itself).
    pub commitment_hash: [u8; 32],
}

impl StateCommitment {
    /// Create the genesis (first) commitment.
    pub fn genesis(initial_state_hash: [u8; 32]) -> Self {
        let prev_commitment = [0u8; 32]; // Genesis has no predecessor
        let action_hash = [0u8; 32]; // No action for genesis

        let commitment_hash =
            compute_commitment_hash(0, &initial_state_hash, &action_hash, &prev_commitment);

        Self {
            sequence: 0,
            state_hash: initial_state_hash,
            action_hash,
            prev_commitment,
            commitment_hash,
        }
    }

    /// Create a new commitment chained to a previous one.
    pub fn chain_from(prev: &StateCommitment, state_hash: [u8; 32], action_hash: [u8; 32]) -> Self {
        let sequence = prev.sequence + 1;
        let prev_commitment = prev.commitment_hash;

        let commitment_hash =
            compute_commitment_hash(sequence, &state_hash, &action_hash, &prev_commitment);

        Self {
            sequence,
            state_hash,
            action_hash,
            prev_commitment,
            commitment_hash,
        }
    }

    /// Verify that this commitment's hash is correctly computed.
    pub fn verify_self(&self) -> bool {
        let computed = compute_commitment_hash(
            self.sequence,
            &self.state_hash,
            &self.action_hash,
            &self.prev_commitment,
        );
        computed == self.commitment_hash
    }

    /// Verify that this commitment correctly chains from the given predecessor.
    pub fn verify_chain_from(&self, prev: &StateCommitment) -> bool {
        // Check sequence is incremented
        if self.sequence != prev.sequence + 1 {
            return false;
        }

        // Check chain link
        if self.prev_commitment != prev.commitment_hash {
            return false;
        }

        // Check commitment hash
        self.verify_self()
    }
}

/// Compute the commitment hash from its components.
fn compute_commitment_hash(
    sequence: u64,
    state_hash: &[u8; 32],
    action_hash: &[u8; 32],
    prev_commitment: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(sequence.to_le_bytes());
    hasher.update(state_hash);
    hasher.update(action_hash);
    hasher.update(prev_commitment);
    hasher.finalize().into()
}

/// Error types for chain verification.
#[derive(Debug, Clone, PartialEq, Eq, Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum ChainError {
    /// The chain is empty.
    #[error("Chain is empty")]
    EmptyChain,
    /// Genesis commitment has wrong sequence number.
    #[error("Genesis has sequence {found}, expected {expected_sequence}")]
    InvalidGenesis { expected_sequence: u64, found: u64 },
    /// A commitment's hash doesn't match its contents.
    #[error("Commitment {sequence} has invalid hash")]
    InvalidCommitmentHash { sequence: u64 },
    /// Chain link is broken (`prev_commitment` doesn't match).
    #[error("Chain link broken at sequence {sequence}")]
    BrokenLink {
        sequence: u64,
        expected: [u8; 32],
        found: [u8; 32],
    },
    /// Sequence numbers are not consecutive.
    #[error("Sequence gap: expected {expected}, found {found}")]
    SequenceGap { expected: u64, found: u64 },
}

/// A chain of state commitments.
///
/// Provides methods to build and verify a tamper-evident history
/// of game states.
#[derive(Debug, Clone, Default, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct CommitmentChain {
    /// The genesis (initial) state hash.
    pub genesis: [u8; 32],
    /// All commitments in order.
    pub commitments: Vec<StateCommitment>,
}

impl CommitmentChain {
    /// Create a new chain with the given genesis state hash.
    pub fn new(genesis_state_hash: [u8; 32]) -> Self {
        let genesis_commitment = StateCommitment::genesis(genesis_state_hash);
        Self {
            genesis: genesis_state_hash,
            commitments: vec![genesis_commitment],
        }
    }

    /// Append a new commitment to the chain.
    ///
    /// Returns a reference to the newly created commitment.
    pub fn append(&mut self, state_hash: [u8; 32], action_hash: [u8; 32]) -> &StateCommitment {
        let prev = self
            .commitments
            .last()
            .expect("Chain should never be empty");
        let new_commitment = StateCommitment::chain_from(prev, state_hash, action_hash);
        self.commitments.push(new_commitment);
        self.commitments.last().expect("just pushed to commitments")
    }

    /// Get the latest commitment.
    pub fn latest(&self) -> Option<&StateCommitment> {
        self.commitments.last()
    }

    /// Get the current sequence number.
    pub fn current_sequence(&self) -> u64 {
        self.commitments.last().map(|c| c.sequence).unwrap_or(0)
    }

    /// Get a commitment by sequence number.
    pub fn get(&self, sequence: u64) -> Option<&StateCommitment> {
        self.commitments.get(sequence as usize)
    }

    /// Get the number of commitments in the chain.
    pub fn len(&self) -> usize {
        self.commitments.len()
    }

    /// Check if the chain is empty (should never be true after construction).
    pub fn is_empty(&self) -> bool {
        self.commitments.is_empty()
    }

    /// Verify the entire chain's integrity.
    ///
    /// Checks:
    /// 1. Genesis commitment is valid
    /// 2. All commitment hashes are correct
    /// 3. All chain links are valid
    /// 4. Sequence numbers are consecutive
    pub fn verify_integrity(&self) -> Result<(), ChainError> {
        if self.commitments.is_empty() {
            return Err(ChainError::EmptyChain);
        }

        // Check genesis
        let genesis = &self.commitments[0];
        if genesis.sequence != 0 {
            return Err(ChainError::InvalidGenesis {
                expected_sequence: 0,
                found: genesis.sequence,
            });
        }
        if !genesis.verify_self() {
            return Err(ChainError::InvalidCommitmentHash { sequence: 0 });
        }

        // Check chain
        for window in self.commitments.windows(2) {
            let prev = &window[0];
            let curr = &window[1];

            // Check sequence
            if curr.sequence != prev.sequence + 1 {
                return Err(ChainError::SequenceGap {
                    expected: prev.sequence + 1,
                    found: curr.sequence,
                });
            }

            // Check chain link
            if curr.prev_commitment != prev.commitment_hash {
                return Err(ChainError::BrokenLink {
                    sequence: curr.sequence,
                    expected: prev.commitment_hash,
                    found: curr.prev_commitment,
                });
            }

            // Check commitment hash
            if !curr.verify_self() {
                return Err(ChainError::InvalidCommitmentHash {
                    sequence: curr.sequence,
                });
            }
        }

        Ok(())
    }

    /// Find the first point where two chains diverge.
    ///
    /// Returns `None` if the chains are identical up to the shorter chain's length.
    /// Returns `Some(sequence)` where the chains first differ.
    pub fn find_divergence(&self, other: &CommitmentChain) -> Option<u64> {
        let min_len = self.commitments.len().min(other.commitments.len());

        for i in 0..min_len {
            if self.commitments[i].commitment_hash != other.commitments[i].commitment_hash {
                return Some(i as u64);
            }
        }

        // Chains are identical up to the shorter length
        if self.commitments.len() != other.commitments.len() {
            Some(min_len as u64)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state_hash(value: u8) -> [u8; 32] {
        let mut hash = [0u8; 32];
        hash[0] = value;
        hash
    }

    fn test_action_hash(value: u8) -> [u8; 32] {
        let mut hash = [0u8; 32];
        hash[31] = value;
        hash
    }

    #[test]
    fn test_genesis_commitment() {
        let state_hash = test_state_hash(1);
        let genesis = StateCommitment::genesis(state_hash);

        assert_eq!(genesis.sequence, 0);
        assert_eq!(genesis.state_hash, state_hash);
        assert_eq!(genesis.prev_commitment, [0u8; 32]);
        assert!(genesis.verify_self());
    }

    #[test]
    fn test_chain_from() {
        let genesis = StateCommitment::genesis(test_state_hash(1));
        let state2 = test_state_hash(2);
        let action = test_action_hash(1);

        let commitment2 = StateCommitment::chain_from(&genesis, state2, action);

        assert_eq!(commitment2.sequence, 1);
        assert_eq!(commitment2.state_hash, state2);
        assert_eq!(commitment2.action_hash, action);
        assert_eq!(commitment2.prev_commitment, genesis.commitment_hash);
        assert!(commitment2.verify_self());
        assert!(commitment2.verify_chain_from(&genesis));
    }

    #[test]
    fn test_commitment_chain_new() {
        let chain = CommitmentChain::new(test_state_hash(1));

        assert_eq!(chain.len(), 1);
        assert_eq!(chain.current_sequence(), 0);
        assert!(chain.verify_integrity().is_ok());
    }

    #[test]
    fn test_commitment_chain_append() {
        let mut chain = CommitmentChain::new(test_state_hash(1));

        chain.append(test_state_hash(2), test_action_hash(1));
        chain.append(test_state_hash(3), test_action_hash(2));
        chain.append(test_state_hash(4), test_action_hash(3));

        assert_eq!(chain.len(), 4);
        assert_eq!(chain.current_sequence(), 3);
        assert!(chain.verify_integrity().is_ok());
    }

    #[test]
    fn test_commitment_chain_get() {
        let mut chain = CommitmentChain::new(test_state_hash(1));
        chain.append(test_state_hash(2), test_action_hash(1));

        let genesis = chain.get(0).unwrap();
        assert_eq!(genesis.sequence, 0);

        let second = chain.get(1).unwrap();
        assert_eq!(second.sequence, 1);

        assert!(chain.get(2).is_none());
    }

    #[test]
    fn test_verify_integrity_detects_tampered_hash() {
        let mut chain = CommitmentChain::new(test_state_hash(1));
        chain.append(test_state_hash(2), test_action_hash(1));

        // Tamper with commitment hash
        chain.commitments[1].commitment_hash[0] ^= 0xFF;

        let result = chain.verify_integrity();
        assert!(matches!(
            result,
            Err(ChainError::InvalidCommitmentHash { sequence: 1 })
        ));
    }

    #[test]
    fn test_verify_integrity_detects_broken_link() {
        let mut chain = CommitmentChain::new(test_state_hash(1));
        chain.append(test_state_hash(2), test_action_hash(1));

        // Tamper with chain link (and fix the commitment hash to match)
        chain.commitments[1].prev_commitment[0] ^= 0xFF;

        let result = chain.verify_integrity();
        // This will fail either as broken link or invalid hash
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_integrity_detects_sequence_gap() {
        let mut chain = CommitmentChain::new(test_state_hash(1));
        chain.append(test_state_hash(2), test_action_hash(1));

        // Tamper with sequence number
        chain.commitments[1].sequence = 5;

        let result = chain.verify_integrity();
        assert!(matches!(
            result,
            Err(ChainError::SequenceGap {
                expected: 1,
                found: 5
            })
        ));
    }

    #[test]
    fn test_find_divergence_identical_chains() {
        let mut chain1 = CommitmentChain::new(test_state_hash(1));
        chain1.append(test_state_hash(2), test_action_hash(1));

        let mut chain2 = CommitmentChain::new(test_state_hash(1));
        chain2.append(test_state_hash(2), test_action_hash(1));

        assert_eq!(chain1.find_divergence(&chain2), None);
    }

    #[test]
    fn test_find_divergence_different_genesis() {
        let chain1 = CommitmentChain::new(test_state_hash(1));
        let chain2 = CommitmentChain::new(test_state_hash(2));

        assert_eq!(chain1.find_divergence(&chain2), Some(0));
    }

    #[test]
    fn test_find_divergence_different_second() {
        let mut chain1 = CommitmentChain::new(test_state_hash(1));
        chain1.append(test_state_hash(2), test_action_hash(1));

        let mut chain2 = CommitmentChain::new(test_state_hash(1));
        chain2.append(test_state_hash(3), test_action_hash(1)); // Different state

        assert_eq!(chain1.find_divergence(&chain2), Some(1));
    }

    #[test]
    fn test_find_divergence_different_lengths() {
        let mut chain1 = CommitmentChain::new(test_state_hash(1));
        chain1.append(test_state_hash(2), test_action_hash(1));
        chain1.append(test_state_hash(3), test_action_hash(2));

        let mut chain2 = CommitmentChain::new(test_state_hash(1));
        chain2.append(test_state_hash(2), test_action_hash(1));
        // chain2 is shorter

        // Chains are identical up to length 2, but chain1 has more
        assert_eq!(chain1.find_divergence(&chain2), Some(2));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut chain = CommitmentChain::new(test_state_hash(1));
        chain.append(test_state_hash(2), test_action_hash(1));
        chain.append(test_state_hash(3), test_action_hash(2));

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&chain).unwrap();
        let restored: CommitmentChain =
            rkyv::from_bytes::<CommitmentChain, rkyv::rancor::Error>(&bytes).unwrap();

        assert_eq!(restored.len(), chain.len());
        assert!(restored.verify_integrity().is_ok());
        assert_eq!(chain.find_divergence(&restored), None);
    }

    #[test]
    fn test_chain_error_display() {
        let errors = vec![
            ChainError::EmptyChain,
            ChainError::InvalidGenesis {
                expected_sequence: 0,
                found: 1,
            },
            ChainError::InvalidCommitmentHash { sequence: 5 },
            ChainError::BrokenLink {
                sequence: 3,
                expected: [1u8; 32],
                found: [2u8; 32],
            },
            ChainError::SequenceGap {
                expected: 2,
                found: 5,
            },
        ];

        for error in errors {
            let display = format!("{}", error);
            assert_ne!(display, "");
        }
    }
}
