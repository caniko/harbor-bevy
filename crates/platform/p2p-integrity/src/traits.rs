//! Core traits for the cheat detection framework.
//!
//! These traits define the interfaces that game-specific and tournament-specific
//! code must implement to use the framework.

/// Trait for actions that can produce a deterministic signing payload.
///
/// Replaces `serde_json` serialization in `SignedAction::sign()`.
/// Each game action type implements this to define how it's represented
/// in cryptographic signatures.
pub trait SignableAction {
    /// Produce a deterministic byte representation for signing.
    ///
    /// This MUST be deterministic: the same action always produces
    /// the same bytes. The bytes should include all fields that
    /// affect game state.
    fn signing_bytes(&self) -> Vec<u8>;
}
