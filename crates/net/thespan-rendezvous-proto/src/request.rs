//! Client → server request payloads.
//!
//! Mutating requests (Register, Unregister, Heartbeat) are wrapped in
//! [`crate::SignedEnvelope`] before transmission and rkyv-serialize the
//! payload struct as the envelope's `payload` bytes.

use crate::namespace::Namespace;
use crate::ticket::PeerTicket;

/// Announce a peer/lobby under a namespace.
///
/// `ttl_secs` is advisory — the server clamps to
/// [`crate::MAX_REGISTRATION_TTL_SECS`]. Re-registration of the same
/// `(signer_peer_id, namespace)` overwrites the prior ticket.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct RegisterRequest {
    pub namespace: Namespace,
    pub ticket: PeerTicket,
    pub ttl_secs: u32,
    /// Opaque application metadata. The server never inspects it.
    pub metadata: Vec<u8>,
}

/// Withdraw a registration early. Idempotent.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct UnregisterRequest {
    pub namespace: Namespace,
}

/// Extend the TTL of a prior registration. The signer in the envelope
/// must match the registration's signer.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct HeartbeatRequest {
    pub namespace: Namespace,
    /// New TTL request, again clamped to [`crate::MAX_REGISTRATION_TTL_SECS`].
    pub ttl_secs: u32,
}

/// List registrations under a namespace. Unsigned — discovery is public.
///
/// Pagination: callers pass back `cursor` from the previous response to
/// continue. The cursor is opaque (server-defined). Empty cursor means
/// "from the start".
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DiscoverRequest {
    pub namespace: Namespace,
    pub cursor: Vec<u8>,
    /// Caller-requested page size. Server clamps to
    /// [`crate::MAX_DISCOVER_LIMIT`].
    pub limit: u32,
}
