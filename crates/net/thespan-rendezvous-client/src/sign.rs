//! Helpers to construct `SignedEnvelopes` against a libp2p keypair.

use libp2p::identity::Keypair;
use rand::Rng;
use std::time::{SystemTime, UNIX_EPOCH};
use thespan_rendezvous_proto::{PeerIdBytes, SignedEnvelope};

/// Wrap any rkyv-serializable payload into a signed envelope.
pub fn seal<T>(key: &Keypair, payload: &T) -> Result<SignedEnvelope, SignError>
where
    T: rkyv::Archive
        + for<'a> rkyv::Serialize<
            rkyv::api::high::HighSerializer<
                rkyv::util::AlignedVec,
                rkyv::ser::allocator::ArenaHandle<'a>,
                rkyv::rancor::Error,
            >,
        >,
{
    let payload_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(payload)
        .map_err(|e| SignError::Serialize(e.to_string()))?
        .to_vec();
    let signature = key
        .sign(&payload_bytes)
        .map_err(|e| SignError::Sign(e.to_string()))?;
    let mut nonce = [0u8; 16];
    rand::rng().fill_bytes(&mut nonce);
    let issued_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let signer = key.public().encode_protobuf();
    let signer_peer_id = PeerIdBytes(key.public().to_peer_id().to_bytes());
    Ok(SignedEnvelope::new(
        signer,
        signer_peer_id,
        nonce,
        issued_at,
        payload_bytes,
        signature,
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error("rkyv serialize failed: {0}")]
    Serialize(String),
    #[error("libp2p sign failed: {0}")]
    Sign(String),
}
