//! Signed envelope for peer-authenticated requests.
//!
//! Anything that mutates server state (Register / Unregister / Heartbeat) is
//! wrapped in a [`SignedEnvelope`]. The signature is over the rkyv bytes of
//! the inner payload, produced with the registering peer's libp2p keypair.
//!
//! This crate does not implement signing/verification — those live in the
//! client and server crates respectively, which depend on `libp2p-identity`.
//! The envelope here is a transport-neutral container.

use crate::ticket::PeerIdBytes;

/// A payload (already rkyv-serialized into bytes) bundled with the public key
/// and signature that authenticate it. Discrimination of which payload type
/// lives inside is carried by the outer [`crate::Frame`] variant.
///
/// Verification, performed server-side:
/// 1. Decode `signer` into a libp2p `PublicKey`.
/// 2. Confirm `PublicKey::to_peer_id().to_bytes() == signer_peer_id`.
/// 3. Verify `signature` over `payload` using that public key.
/// 4. Reject if `(signer_peer_id, nonce)` was already seen within the
///    replay window.
/// 5. Reject if `issued_at` deviates from server clock by more than the
///    configured skew tolerance.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct SignedEnvelope {
    /// Public key bytes in libp2p protobuf encoding (`PublicKey::encode_protobuf`).
    pub signer: Vec<u8>,
    /// Peer id derived from `signer`. Duplicated here so the server can
    /// index/dedupe by peer id without parsing the key first.
    pub signer_peer_id: PeerIdBytes,
    /// Random per-request nonce, anti-replay.
    pub nonce: [u8; 16],
    /// Unix-seconds at which the envelope was produced.
    pub issued_at: u64,
    /// rkyv-serialized payload bytes, exactly as fed to the signer.
    pub payload: Vec<u8>,
    /// Detached signature over `payload`.
    pub signature: Vec<u8>,
}

impl SignedEnvelope {
    pub fn new(
        signer: Vec<u8>,
        signer_peer_id: PeerIdBytes,
        nonce: [u8; 16],
        issued_at: u64,
        payload: Vec<u8>,
        signature: Vec<u8>,
    ) -> Self {
        Self {
            signer,
            signer_peer_id,
            nonce,
            issued_at,
            payload,
            signature,
        }
    }
}
