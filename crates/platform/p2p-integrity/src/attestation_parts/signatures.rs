// Cryptographic attestation for game actions.
//
// This module provides Ed25519 digital signatures for game actions,
// enabling non-repudiation and tamper detection.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::traits::SignableAction;

/// A player's cryptographic key pair for signing actions.
///
/// The private key is never serialized; only the public key is shared.
///
/// # Security
///
/// `Debug` output intentionally includes only the public key. Keep
/// `signing_key` private and do not serialize this type directly.
#[derive(Clone)]
pub struct PlayerKeyPair {
    signing_key: SigningKey,
    /// The public key, shared with other participants.
    pub public_key: [u8; 32],
}

impl PlayerKeyPair {
    /// Generate a new random key pair.
    ///
    /// # Panics
    ///
    /// Panics if the operating system random number generator fails. Callers
    /// that need recoverable entropy failures should construct an
    /// `ed25519_dalek::SigningKey` themselves and use [`Self::from_signing_key`].
    ///
    /// # Examples
    ///
    /// ```
    /// use p2p_integrity::PlayerKeyPair;
    ///
    /// let key_pair = PlayerKeyPair::generate();
    /// let signature = key_pair.sign(b"action bytes");
    ///
    /// assert_eq!(signature.len(), 64);
    /// ```
    pub fn generate() -> Self {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("Failed to generate random bytes");
        let signing_key = SigningKey::from_bytes(&secret);
        let public_key = signing_key.verifying_key().to_bytes();
        Self {
            signing_key,
            public_key,
        }
    }

    /// Create a key pair from an existing signing key.
    ///
    /// # Examples
    ///
    /// ```
    /// use p2p_integrity::PlayerKeyPair;
    /// use ed25519_dalek::SigningKey;
    ///
    /// let signing_key = SigningKey::from_bytes(&[7; 32]);
    /// let key_pair = PlayerKeyPair::from_signing_key(signing_key);
    ///
    /// assert_ne!(key_pair.public_key, [0; 32]);
    /// ```
    pub fn from_signing_key(signing_key: SigningKey) -> Self {
        let public_key = signing_key.verifying_key().to_bytes();
        Self {
            signing_key,
            public_key,
        }
    }

    /// Create a key pair from a secret seed (32 bytes).
    ///
    /// # Security
    ///
    /// The seed is the private key material. Use this only for deterministic
    /// fixtures or when the caller already has a securely stored seed.
    ///
    /// # Examples
    ///
    /// ```
    /// use p2p_integrity::PlayerKeyPair;
    ///
    /// let first = PlayerKeyPair::from_seed(&[3; 32]);
    /// let second = PlayerKeyPair::from_seed(&[3; 32]);
    ///
    /// assert_eq!(first.public_key, second.public_key);
    /// ```
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(seed);
        let public_key = signing_key.verifying_key().to_bytes();
        Self {
            signing_key,
            public_key,
        }
    }

    /// Sign arbitrary bytes.
    ///
    /// # Security
    ///
    /// The caller is responsible for passing deterministic bytes that include
    /// every state-changing field. Prefer [`SignedAction::sign`] for game
    /// actions because it binds metadata and the previous hash too.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        let signature = self.signing_key.sign(message);
        signature.to_bytes()
    }

    /// Derive the local session-HMAC key associated with this player identity.
    ///
    /// # Security
    ///
    /// This derives a deterministic local key from the public identity only. It
    /// is not a replacement for a shared secret or authenticated key exchange.
    pub fn session_hmac_key(&self) -> [u8; 32] {
        *blake3::hash(&self.public_key).as_bytes()
    }
}

impl std::fmt::Debug for PlayerKeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Don't expose the private key
        f.debug_struct("PlayerKeyPair")
            .field("public_key", &hex::encode(self.public_key))
            .finish()
    }
}

/// Hex encoding for debug output (simple implementation).
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}

/// Metadata about a signed action.
#[derive(Debug, Clone, Copy, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ActionMetadata {
    /// Monotonically increasing sequence number.
    pub sequence: u64,
    /// Timestamp in milliseconds since match start.
    pub timestamp_ms: u64,
    /// Match identifier (UUID as bytes).
    pub match_id: [u8; 16],
    /// Player index (0 or 1 for 1v1 games).
    pub player_index: u8,
}

/// A signed game action with chain link for tamper detection.
///
/// Each signed action includes:
/// - The action itself (generic type A)
/// - Metadata (sequence, timestamp, match ID, player)
/// - Link to previous state hash (for chaining)
/// - Ed25519 signature
/// - Signer's public key
///
/// # Type Parameters
///
/// - `A`: The action type (must implement `SignableAction` for signing)
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct SignedAction<A> {
    /// The game action.
    pub action: A,
    /// Action metadata.
    pub metadata: ActionMetadata,
    /// Hash of the previous state (chain link).
    pub prev_hash: [u8; 32],
    /// Ed25519 signature of the action.
    pub signature: [u8; 64],
    /// Public key of the signer.
    pub public_key: [u8; 32],
}

impl<A: SignableAction> SignedAction<A> {
    /// Sign an action with the given key pair.
    ///
    /// The signature covers:
    /// - Action's signing bytes (deterministic, via `SignableAction`)
    /// - Metadata (sequence, timestamp, `match_id`, `player_index`)
    /// - Previous state hash
    ///
    /// # Security
    ///
    /// Verification only proves that the holder of the private key signed the
    /// deterministic action bytes, metadata, and `prev_hash`. It does not prove
    /// the action is legal; callers must still validate the action against game
    /// rules.
    pub fn sign(
        action: A,
        metadata: ActionMetadata,
        prev_hash: [u8; 32],
        key_pair: &PlayerKeyPair,
    ) -> Self {
        let message = Self::compute_signing_message(&action, &metadata, &prev_hash);
        let signature = key_pair.sign(&message);

        Self {
            action,
            metadata,
            prev_hash,
            signature,
            public_key: key_pair.public_key,
        }
    }

    /// Compute the message to be signed.
    fn compute_signing_message(
        action: &A,
        metadata: &ActionMetadata,
        prev_hash: &[u8; 32],
    ) -> Vec<u8> {
        let mut message = action.signing_bytes();

        // Add metadata
        message.extend_from_slice(&metadata.sequence.to_le_bytes());
        message.extend_from_slice(&metadata.timestamp_ms.to_le_bytes());
        message.extend_from_slice(&metadata.match_id);
        message.push(metadata.player_index);

        // Add chain link
        message.extend_from_slice(prev_hash);

        message
    }

    /// Compute the hash of this signed action.
    ///
    /// This hash can be used as input for the next action's `prev_hash`.
    pub fn compute_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();

        // Hash the action
        hasher.update(self.action.signing_bytes());

        // Hash metadata
        hasher.update(self.metadata.sequence.to_le_bytes());
        hasher.update(self.metadata.timestamp_ms.to_le_bytes());
        hasher.update(self.metadata.match_id);
        hasher.update([self.metadata.player_index]);

        // Hash chain link
        hasher.update(self.prev_hash);

        // Hash signature (makes the hash unique to this signed version)
        hasher.update(self.signature);

        hasher.finalize().into()
    }

    /// Returns `true` if signature and public key are both zeroed.
    ///
    /// Zeroed fields indicate the action was recorded without signing
    /// (e.g. opponent moves where we don't have their private key).
    /// Integrity for unsigned actions is provided by the commitment chain.
    pub fn is_unsigned(&self) -> bool {
        self.signature == [0u8; 64] && self.public_key == [0u8; 32]
    }

    /// Verify the signature is valid.
    ///
    /// Returns `true` if the signature was created by the holder of
    /// the private key corresponding to `self.public_key`.
    ///
    /// Returns `false` for malformed public keys or signatures that do not
    /// match the signed message. This method does not panic.
    ///
    /// # Panics
    ///
    /// This method does not panic for malformed keys, zeroed unsigned sentinels,
    /// or mismatched signatures; all verification failures return `false`.
    ///
    /// # Security
    ///
    /// Unsigned sentinel actions return `false`; use [`Self::is_unsigned`] when
    /// accepting locally recorded unsigned actions through another integrity
    /// mechanism.
    pub fn verify(&self) -> bool {
        // Reconstruct the message
        let message = Self::compute_signing_message(&self.action, &self.metadata, &self.prev_hash);

        // Parse the public key
        let Ok(verifying_key) = VerifyingKey::from_bytes(&self.public_key) else {
            return false;
        };

        // Parse the signature
        let signature = Signature::from_bytes(&self.signature);

        // Verify
        verifying_key.verify(&message, &signature).is_ok()
    }
}

/// Verify a signature given the raw components.
///
/// Useful for verifying signatures without the full `SignedAction` struct.
///
/// Returns `false` for malformed public keys or signatures that do not match
/// `message`. This function does not panic.
///
/// # Examples
///
/// ```
/// use p2p_integrity::{PlayerKeyPair, attestation::verify_signature};
///
/// let key_pair = PlayerKeyPair::from_seed(&[9; 32]);
/// let message = b"deterministic action bytes";
/// let signature = key_pair.sign(message);
///
/// assert!(verify_signature(message, &signature, &key_pair.public_key));
/// assert!(!verify_signature(b"tampered", &signature, &key_pair.public_key));
/// ```
///
/// # Security
///
/// This verifies only the exact bytes provided by the caller. The caller must
/// construct those bytes deterministically and include all security-relevant
/// fields.
///
/// # Panics
///
/// This function does not panic for malformed public keys or invalid
/// signatures; all verification failures return `false`.
pub fn verify_signature(message: &[u8], signature: &[u8; 64], public_key: &[u8; 32]) -> bool {
    let Ok(verifying_key) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };

    let sig = Signature::from_bytes(signature);

    verifying_key.verify(message, &sig).is_ok()
}
