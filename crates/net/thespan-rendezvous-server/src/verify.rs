//! `SignedEnvelope` verification.

use libp2p::identity::PublicKey;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thespan_rendezvous_proto::{ErrorCode, ProtoError, SignedEnvelope};

pub fn verify(
    env: &SignedEnvelope,
    now: SystemTime,
    max_skew: Duration,
) -> Result<PublicKey, ProtoError> {
    let now_secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let issued = env.issued_at;
    let skew = now_secs.abs_diff(issued);
    if skew > max_skew.as_secs() {
        return Err(ProtoError::new(
            ErrorCode::Stale,
            format!("skew {skew}s > {}s", max_skew.as_secs()),
        ));
    }

    let pk = PublicKey::try_decode_protobuf(&env.signer)
        .map_err(|e| ProtoError::new(ErrorCode::AuthFailed, format!("decode pk: {e}")))?;

    let derived = pk.to_peer_id();
    if derived.to_bytes() != env.signer_peer_id.0 {
        return Err(ProtoError::new(
            ErrorCode::AuthFailed,
            "signer_peer_id mismatch",
        ));
    }

    if !pk.verify(&env.payload, &env.signature) {
        return Err(ProtoError::new(ErrorCode::AuthFailed, "signature invalid"));
    }
    Ok(pk)
}
