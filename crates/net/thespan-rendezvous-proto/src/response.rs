//! Server → client response payloads.

use crate::ticket::PeerTicket;

/// Server acknowledgement of a Register / Heartbeat.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct RegisterAck {
    /// Effective TTL after server-side clamping.
    pub granted_ttl_secs: u32,
    /// Server-assigned registration id; clients echo it back in heartbeats
    /// for cheap dedup, though not strictly required (signer+namespace is
    /// also a primary key).
    pub registration_id: u64,
}

/// One row of a discovery result.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct LobbyEntry {
    pub ticket: PeerTicket,
    /// Opaque metadata as supplied at Register time.
    pub metadata: Vec<u8>,
    /// Seconds until this entry expires absent a heartbeat.
    pub ttl_remaining_secs: u32,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DiscoverResponse {
    pub entries: Vec<LobbyEntry>,
    /// Empty if there are no further pages.
    pub next_cursor: Vec<u8>,
}
