//! libp2p Transport over Steam Relay using `ISteamNetworkingSockets`.
//!
//! Provides a [`SteamTransport`] that implements [`libp2p_core::Transport`],
//! enabling Steam P2P relay connections to be used as a libp2p transport.
//!
//! # Lane-Aware Multiplexing
//!
//! Instead of Noise + Yamux, this crate provides [`SteamLaneMuxer`] — a custom
//! `StreamMuxer` that maps each libp2p substream to its own Steam lane. Each
//! lane with reliable delivery is an independently ordered byte stream, replacing
//! both Yamux (multiplexing) and Noise (Steam Datagram Relay provides encryption).
//!
//! # Architecture
//!
//! - **Listen path**: `spawn_blocking` bridge blocks on `ListenSocket::events()`
//!   (0 CPU idle), forwards accepted `NetConnection` via `tokio::sync::mpsc`.
//! - **Dial path**: `spawn_blocking` task polls `get_connection_info()` at 16ms
//!   intervals (~1s lifetime), completes a `oneshot` channel.
//! - **Muxer**: Each substream gets its own Steam lane. Lane pool grows on demand.

mod dial;
mod error;
pub mod lane;
mod listener;
pub mod muxer;
mod protocol;
pub(crate) mod stream;
#[cfg(test)]
mod tests;
mod transport;

pub use error::SteamTransportError;
pub use lane::LaneConfig;
pub use muxer::{SteamLaneMuxer, SteamMuxerError, peer_id_handshake};
pub use protocol::{parse_steam_id, steam_multiaddr};
pub use stream::SteamStream;
pub use transport::{SteamTransport, SteamTransportConfig};

// Re-export steamworks types used in the public API so consumers
// don't need a direct steamworks dependency.
pub use steamworks::networking_sockets::NetworkingSockets;
pub use steamworks::networking_utils::NetworkingUtils;

// Shared constants used by both stream.rs and muxer.rs
pub(crate) const TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);
pub(crate) const READ_BATCH_SIZE: usize = 64;
