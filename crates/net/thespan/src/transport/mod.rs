//! Transport implementations.

use std::time::Duration;

mod error;
pub mod lan;
pub mod libp2p_internet;
mod message_kind;
#[cfg(feature = "steam")]
pub mod steam;
pub mod webrtc;

pub use error::TransportError;
pub use message_kind::{
    DIRECT_SESSION_MAX_BYTES, GAMEPLAY_MESSAGE_MAX_BYTES, GOSSIPSUB_MAX_TRANSMIT_SIZE,
    STATE_SYNC_MESSAGE_MAX_BYTES, TransportMessageKind,
};

/// Explicit libp2p idle timeout for non-Steam swarms.
///
/// The transport behaviours also send ping/keep-alive traffic; this timeout
/// tolerates longer idle periods than the QUIC default.
pub const SWARM_IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);
