//! Wire protocol for the Thespan libp2p rendezvous service.
//!
//! Deliberately has no `libp2p` dependency. `PeerIds` and multiaddrs travel
//! as opaque byte/string forms so the protocol stays auditable, can be
//! re-implemented in any language, and can evolve independently of the
//! libp2p version pinned by the client/server crates.
//!
//! Protocol identifier sent on a libp2p stream: see [`PROTOCOL_ID`].
//! Wire format: rkyv-archived [`Frame`], length-prefixed by the libp2p
//! request-response codec on the transport side.

pub mod envelope;
pub mod error;
pub mod namespace;
pub mod request;
pub mod response;
pub mod ticket;

pub use envelope::SignedEnvelope;
pub use error::{ErrorCode, ProtoError};
pub use namespace::Namespace;
pub use request::{DiscoverRequest, HeartbeatRequest, RegisterRequest, UnregisterRequest};
pub use response::{DiscoverResponse, LobbyEntry, RegisterAck};
pub use ticket::{MultiaddrBytes, PeerIdBytes, PeerTicket, RelayHint};

/// libp2p protocol id negotiated on the rendezvous stream.
///
/// Versioned. Bump the trailing integer on incompatible wire changes.
pub const PROTOCOL_ID: &str = "/thespan/rendezvous/2";

/// Maximum size of any single wire frame, bytes. Server enforces this.
/// Picked to comfortably fit a [`DiscoverResponse`] with [`MAX_DISCOVER_LIMIT`]
/// entries even with several multiaddrs each.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Maximum entries the server will return for one Discover request.
pub const MAX_DISCOVER_LIMIT: u32 = 64;

/// Maximum TTL a client may request for a registration, seconds.
/// Server clamps anything above this. Heartbeats extend.
pub const MAX_REGISTRATION_TTL_SECS: u32 = 300;

/// Top-level wire frame. One of these per stream message.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum Frame {
    Register(SignedEnvelope),
    Unregister(SignedEnvelope),
    Heartbeat(SignedEnvelope),
    Discover(DiscoverRequest),

    RegisterAck(RegisterAck),
    DiscoverResponse(DiscoverResponse),
    Error(ProtoError),
}
