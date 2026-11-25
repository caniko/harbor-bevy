// Witness protocol for third-party verification.
//
// This module provides the types and logic for witness attestations
// and managing witness sessions.

use crate::attestation::verify_signature;

/// Witness attestation of a game action.
///
/// A witness observes the game and signs their verdict on each action.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct WitnessAttestation {
    /// Match identifier (UUID as bytes).
    pub match_id: [u8; 16],
    /// Action sequence number being attested.
    pub action_sequence: u64,
    /// Hash of the action being attested.
    pub action_hash: [u8; 32],
    /// Witness identifier (UUID as bytes).
    pub witness_id: [u8; 16],
    /// Witness's public key.
    pub witness_public_key: [u8; 32],
    /// The witness's verdict.
    pub verdict: WitnessVerdict,
    /// Signature over (`match_id` || `action_sequence` || `action_hash` || verdict).
    pub signature: [u8; 64],
}

impl WitnessAttestation {
    /// Compute the message to be signed.
    fn compute_signing_message(
        match_id: &[u8; 16],
        action_sequence: u64,
        action_hash: &[u8; 32],
        verdict: &WitnessVerdict,
    ) -> Vec<u8> {
        let mut message = Vec::new();
        message.extend_from_slice(match_id);
        message.extend_from_slice(&action_sequence.to_le_bytes());
        message.extend_from_slice(action_hash);
        verdict.signing_bytes(&mut message);
        message
    }

    /// Verify the attestation signature.
    pub fn verify(&self) -> bool {
        let message = Self::compute_signing_message(
            &self.match_id,
            self.action_sequence,
            &self.action_hash,
            &self.verdict,
        );
        verify_signature(&message, &self.signature, &self.witness_public_key)
    }

    /// Create a new attestation (for witnesses to use).
    ///
    /// The caller must provide their signing function.
    pub fn create(
        match_id: [u8; 16],
        action_sequence: u64,
        action_hash: [u8; 32],
        witness_id: [u8; 16],
        witness_public_key: [u8; 32],
        verdict: WitnessVerdict,
        sign_fn: impl FnOnce(&[u8]) -> [u8; 64],
    ) -> Self {
        let message =
            Self::compute_signing_message(&match_id, action_sequence, &action_hash, &verdict);
        let signature = sign_fn(&message);

        Self {
            match_id,
            action_sequence,
            action_hash,
            witness_id,
            witness_public_key,
            verdict,
            signature,
        }
    }
}

/// A witness's verdict on an action.
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum WitnessVerdict {
    /// Action is valid according to game rules.
    Valid,
    /// Action violates game rules.
    Invalid {
        /// Error code for categorization.
        code: u32,
        /// Human-readable reason.
        reason: String,
    },
    /// Witness cannot determine validity (e.g., hidden information).
    Abstain {
        /// Reason for abstaining.
        reason: String,
    },
}

impl WitnessVerdict {
    /// Check if this is a valid verdict.
    pub fn is_valid(&self) -> bool {
        matches!(self, WitnessVerdict::Valid)
    }

    /// Check if this is an invalid verdict.
    pub fn is_invalid(&self) -> bool {
        matches!(self, WitnessVerdict::Invalid { .. })
    }

    /// Check if this is an abstain verdict.
    pub fn is_abstain(&self) -> bool {
        matches!(self, WitnessVerdict::Abstain { .. })
    }

    /// Produce deterministic bytes for cryptographic signing.
    fn signing_bytes(&self, out: &mut Vec<u8>) {
        match self {
            WitnessVerdict::Valid => out.push(0),
            WitnessVerdict::Invalid { code, reason } => {
                out.push(1);
                out.extend_from_slice(&code.to_le_bytes());
                out.extend_from_slice(reason.as_bytes());
            }
            WitnessVerdict::Abstain { reason } => {
                out.push(2);
                out.extend_from_slice(reason.as_bytes());
            }
        }
    }
}
