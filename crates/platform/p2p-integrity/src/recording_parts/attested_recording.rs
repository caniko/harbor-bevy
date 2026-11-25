// Attested recording with cryptographic verification.
//
// This module provides a recording format that includes cryptographic
// attestations from players and witnesses, enabling post-match verification.

use std::collections::HashMap;
use thiserror::Error;

use crate::attestation::SignedAction;
use crate::commitment::CommitmentChain;
use crate::threshold::ConfidenceLevel;
use crate::traits::SignableAction;
use crate::witness::WitnessAttestation;

/// A recording with cryptographic attestation.
///
/// Contains all evidence needed to verify a match was played fairly.
///
/// # Type Parameters
///
/// - `A`: The action type
#[derive(Debug, Clone)]
pub struct AttestedRecording<A> {
    /// Match identifier (UUID as bytes).
    pub match_id: [u8; 16],
    /// Signed action chain.
    pub actions: Vec<SignedAction<A>>,
    /// State commitment chain.
    pub commitments: CommitmentChain,
    /// Witness attestations by action sequence.
    pub attestations: HashMap<u64, Vec<WitnessAttestation>>,
}

impl<A: SignableAction> AttestedRecording<A> {
    /// Create a new recording.
    pub fn new(match_id: [u8; 16], genesis_state_hash: [u8; 32]) -> Self {
        Self {
            match_id,
            actions: Vec::new(),
            commitments: CommitmentChain::new(genesis_state_hash),
            attestations: HashMap::new(),
        }
    }

    /// Add a signed action to the recording.
    pub fn add_action(&mut self, action: SignedAction<A>, state_hash: [u8; 32]) {
        let action_hash = action.compute_hash();
        self.commitments.append(state_hash, action_hash);
        self.actions.push(action);
    }

    /// Add a witness attestation.
    pub fn add_attestation(&mut self, attestation: WitnessAttestation) {
        self.attestations
            .entry(attestation.action_sequence)
            .or_default()
            .push(attestation);
    }

    /// Validate the recording.
    pub fn validate(&self) -> ValidationReport {
        let mut report = ValidationReport::new(self.match_id);

        // 1. Validate commitment chain
        if let Err(e) = self.commitments.verify_integrity() {
            report.add_error(ValidationError::BrokenCommitmentChain {
                details: e.to_string(),
            });
        }

        // 2. Validate action signatures
        for (i, action) in self.actions.iter().enumerate() {
            if action.is_unsigned() {
                report.add_warning(ValidationWarning::UnsignedAction { sequence: i as u64 });
            } else if !action.verify() {
                report.add_error(ValidationError::InvalidActionSignature { sequence: i as u64 });
            }
        }

        // 3. Validate witness attestations
        for (sequence, attestations) in &self.attestations {
            let mut valid = 0;
            let mut invalid = 0;

            for attestation in attestations {
                if attestation.verify() {
                    match &attestation.verdict {
                        crate::witness::WitnessVerdict::Valid => valid += 1,
                        crate::witness::WitnessVerdict::Invalid { .. } => invalid += 1,
                        crate::witness::WitnessVerdict::Abstain { .. } => {}
                    }
                } else {
                    report.add_warning(ValidationWarning::InvalidWitnessSignature {
                        sequence: *sequence,
                        witness_id: attestation.witness_id,
                    });
                }
            }

            if invalid > valid && valid + invalid > 0 {
                report.add_error(ValidationError::WitnessesDisagreed {
                    sequence: *sequence,
                    valid_count: valid,
                    invalid_count: invalid,
                });
            }
        }

        // 4. Check action count matches commitment count
        // (commitments include genesis, so should be actions.len() + 1)
        if self.commitments.len() != self.actions.len() + 1 {
            report.add_error(ValidationError::ActionCommitmentMismatch {
                actions: self.actions.len(),
                commitments: self.commitments.len(),
            });
        }

        report
    }
}

/// Validation error types.
#[derive(Debug, Clone, Error)]
pub enum ValidationError {
    /// Commitment chain integrity check failed.
    #[error("Commitment chain broken: {details}")]
    BrokenCommitmentChain { details: String },
    /// An action signature is invalid.
    #[error("Invalid signature on action {sequence}")]
    InvalidActionSignature { sequence: u64 },
    /// Majority of witnesses marked action as invalid.
    #[error(
        "Witnesses disagreed on action {sequence}: {valid_count} valid, {invalid_count} invalid"
    )]
    WitnessesDisagreed {
        sequence: u64,
        valid_count: usize,
        invalid_count: usize,
    },
    /// Action count doesn't match commitment count.
    #[error("Action count ({actions}) doesn't match commitment count ({commitments})")]
    ActionCommitmentMismatch { actions: usize, commitments: usize },
}

/// Validation warning types (non-fatal issues).
#[derive(Debug, Clone)]
pub enum ValidationWarning {
    /// A witness signature is invalid.
    InvalidWitnessSignature { sequence: u64, witness_id: [u8; 16] },
    /// Action has no signature (integrity provided by commitment chain).
    UnsignedAction { sequence: u64 },
}

impl std::fmt::Display for ValidationWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationWarning::InvalidWitnessSignature {
                sequence,
                witness_id,
            } => {
                write!(
                    f,
                    "Invalid witness signature on action {} from {:02x?}",
                    sequence, witness_id
                )
            }
            ValidationWarning::UnsignedAction { sequence } => {
                write!(f, "Action {} has no signature (unsigned)", sequence)
            }
        }
    }
}

/// Validation report for a recording.
#[derive(Debug, Clone)]
pub struct ValidationReport {
    /// Match identifier (UUID as bytes).
    pub match_id: [u8; 16],
    /// Critical errors that indicate cheating or corruption.
    pub errors: Vec<ValidationError>,
    /// Warnings that may indicate issues but aren't definitive.
    pub warnings: Vec<ValidationWarning>,
}

impl ValidationReport {
    /// Create a new report.
    pub fn new(match_id: [u8; 16]) -> Self {
        Self {
            match_id,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Add an error.
    pub fn add_error(&mut self, error: ValidationError) {
        self.errors.push(error);
    }

    /// Add a warning.
    pub fn add_warning(&mut self, warning: ValidationWarning) {
        self.warnings.push(warning);
    }

    /// Check if the recording is valid (no errors).
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }

    /// Get the confidence level based on validation results.
    pub fn confidence(&self) -> ConfidenceLevel {
        if !self.errors.is_empty() {
            return ConfidenceLevel::None;
        }

        let has_signature_issues = self
            .warnings
            .iter()
            .any(|warning| matches!(warning, ValidationWarning::UnsignedAction { .. }));

        if has_signature_issues {
            ConfidenceLevel::Low
        } else if self.warnings.is_empty() {
            ConfidenceLevel::High
        } else {
            ConfidenceLevel::Medium
        }
    }

    /// Format a detailed human-readable report of all validation issues.
    pub fn to_detailed_string(&self, action_count: usize, attestation_count: usize) -> String {
        use std::fmt::Write;
        let mut out = String::new();

        let match_id = crate::bytes_to_uuid(self.match_id);
        _ = writeln!(&mut out, "=== Validation Report ===");
        _ = writeln!(&mut out, "Match ID: {match_id}");
        _ = writeln!(&mut out, "Confidence: {}", self.confidence());
        _ = writeln!(&mut out, "Actions: {action_count}");
        _ = writeln!(&mut out, "Attestations: {attestation_count}");
        _ = writeln!(
            &mut out,
            "Result: {}",
            if self.is_valid() { "VALID" } else { "INVALID" }
        );

        if !self.errors.is_empty() {
            _ = writeln!(&mut out, "\n--- Errors ({}) ---", self.errors.len());
            for (i, error) in self.errors.iter().enumerate() {
                _ = writeln!(&mut out, "  {}. {error}", i + 1);
            }
        }

        if !self.warnings.is_empty() {
            _ = writeln!(&mut out, "\n--- Warnings ({}) ---", self.warnings.len());
            for (i, warning) in self.warnings.iter().enumerate() {
                _ = writeln!(&mut out, "  {}. {warning}", i + 1);
            }
        }

        out
    }
}

impl Default for ValidationReport {
    fn default() -> Self {
        Self::new([0u8; 16])
    }
}
