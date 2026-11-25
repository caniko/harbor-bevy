use libp2p::{Multiaddr, PeerId};

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("dial failed after {attempts} attempts peer={peer_id:?} addr={addr:?}: {message}")]
    Dial {
        peer_id: Option<PeerId>,
        addr: Option<Multiaddr>,
        attempts: u8,
        message: String,
    },
    #[error("listen failed addr={addr}: {message}")]
    Listen { addr: Multiaddr, message: String },
    #[error("stream error peer={peer_id:?}: {message}")]
    Stream {
        peer_id: Option<PeerId>,
        message: String,
    },
    #[error("transport setup failed: {0}")]
    Transport(String),
    #[error("behaviour setup failed: {0}")]
    Behaviour(String),
    #[error("reliable event queue full; network actor stopping")]
    ReliableEventOverflow,
    #[error("oversized message kind={kind} peer={peer_id:?}: {observed_bytes} > {limit}")]
    OversizedMessage {
        peer_id: Option<PeerId>,
        kind: &'static str,
        observed_bytes: usize,
        limit: usize,
    },
}
