//! Cryptographic attestation and recording validation for P2P play.
//!
//! This framework provides cryptographic attestation and witness-based verification
//! for detecting cheating in peer-to-peer games. It is designed to be:
//!
//! - **Format-agnostic**: Works with any tournament format or matchmaking system
//! - **Pluggable**: Game-specific validation via traits
//! - **Configurable**: Adjustable trust thresholds per deployment
//!
//! # Core Concepts
//!
//! - **`SignedAction`**: Cryptographically signed game actions with chain links
//! - **`CommitmentChain`**: Tamper-evident history of state transitions
//! - **`WitnessProtocol`**: Third-party verification from honest observers
//! - **`ThresholdVerification`**: Configurable confidence levels
//!
//! # Workflow
//!
//! 1. Sign game actions with `SignedAction::sign()`
//! 2. Verify signatures with `signed.verify()`
//! 3. Build tamper-evident history with `CommitmentChain`
//! 4. Use `WitnessProtocol` for third-party verification

pub mod attestation;
pub mod commitment;
pub mod recording;
pub mod threshold;
pub mod traits;
pub mod witness;

// Re-export main types at crate root
pub use attestation::{ActionMetadata, PlayerKeyPair, SignedAction};
pub use commitment::{ChainError, CommitmentChain, StateCommitment};
pub use recording::{AttestedRecording, ValidationError, ValidationReport, ValidationWarning};
pub use threshold::ConfidenceLevel;
pub use traits::SignableAction;
pub use witness::{WitnessAttestation, WitnessVerdict};

/// Convert a UUID to raw bytes.
pub fn uuid_to_bytes(id: uuid::Uuid) -> [u8; 16] {
    *id.as_bytes()
}

/// Convert raw bytes back to a UUID.
pub fn bytes_to_uuid(bytes: [u8; 16]) -> uuid::Uuid {
    uuid::Uuid::from_bytes(bytes)
}
