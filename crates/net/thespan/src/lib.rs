//! Thespan: standalone multi-transport actor networking library.
//!
//! Built on libp2p. No framework dependency (Bevy, etc.).
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │  Consumer thread                                      │
//! │                                                        │
//! │  App Logic ──→ CommandHandle.send() ──→               │
//! │       ↑                                    │          │
//! │  NetworkHandle.event_rx              piying mailbox     │
//! │       ↑                                    ↓          │
//! ├───────┼────────────────────────────────────┼──────────┤
//! │       │    Background Thread (tokio runtime)│          │
//! │       │                                    │          │
//! │   event_tx ←── NetworkActor (piying) ←── mailbox_rx    │
//! │                    │                                  │
//! │              libp2p Swarm                             │
//! │          (QUIC/TCP + Noise + Yamux)                   │
//! └──────────────────────────────────────────────────────┘
//! ```

pub mod actor;
pub mod config;
pub mod event_channel;
pub mod handle;
pub mod identity;
pub mod session;
pub mod spawn;
pub mod transport;

// Re-exports for convenience
#[cfg(feature = "legacy-raw-codec")]
pub use actor::commands::SendTarget;
pub use actor::commands::{GossipTopic, NetworkCommand};
pub use actor::events::{ActorErrorKind, ActorEvent};
#[cfg(feature = "legacy-raw-codec")]
pub use actor::network::BYE_MARKER;
#[cfg(feature = "steam")]
pub use config::SteamConfig;
pub use config::{LanConfig, ThespanConfig, TransportType, WebRtcConfig};
pub use transport::libp2p_internet::InternetConfig;
pub use transport::{
    DIRECT_SESSION_MAX_BYTES, GAMEPLAY_MESSAGE_MAX_BYTES, GOSSIPSUB_MAX_TRANSMIT_SIZE,
    STATE_SYNC_MESSAGE_MAX_BYTES, TransportError, TransportMessageKind,
};

pub use event_channel::EventReceiver;
pub use handle::{
    CommandHandle, CommandSendError, NETWORK_THREAD_PANICS_TOTAL, NetworkHandle,
    RemoteTellSendError, THESPAN_EVENT_DROPS, THESPAN_RELIABLE_OVERFLOWS,
};
pub use piying::remote::SwarmSender;
pub use spawn::{
    PreRunHook, PreRunWithCleanup, TransportCleanup, ed25519_public_key_from_peer_id,
    spawn_network_actor, spawn_network_actor_with_config, spawn_network_actor_with_hook,
    spawn_network_actor_with_keypair, spawn_with_lifecycle,
};

pub use actor::behavior::{KeepAlivePeerTelemetry, KeepAliveTelemetryHandle};
pub use libp2p::Multiaddr;
pub use libp2p::PeerId;
