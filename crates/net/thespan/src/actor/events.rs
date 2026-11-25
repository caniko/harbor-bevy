//! Events sent from the network actor back to the consumer.

use libp2p::{Multiaddr, PeerId};

use super::commands::GossipTopic;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorErrorKind {
    ListenFailed,
    DialFailed,
}

/// Events that the background network actor pushes to the consumer.
///
/// Received via the event channel returned by `spawn_network_actor`.
#[derive(Debug)]
pub enum ActorEvent {
    /// Received a message from a peer.
    MessageReceived {
        from: PeerId,
        /// Raw bytes of the `WireMessage`.
        payload: Vec<u8>,
    },

    /// A peer has connected.
    PeerConnected { peer_id: PeerId, addr: Multiaddr },

    /// A peer has disconnected.
    PeerDisconnected { peer_id: PeerId, reason: String },

    /// The swarm has started listening on an address.
    ListenStarted { addr: Multiaddr },

    /// An error occurred in the network actor.
    Error { kind: ActorErrorKind, error: String },

    /// An inbound message exceeded its transport-level size budget and was dropped.
    OversizedMessageDropped {
        peer_id: PeerId,
        kind: &'static str,
        observed_bytes: usize,
        limit: usize,
        total_for_peer: u64,
    },

    /// Received a message from a `GossipSub` topic.
    GossipReceived {
        /// The topic the message was published to.
        topic: GossipTopic,
        /// The peer who originally published the message.
        source: Option<PeerId>,
        /// The raw payload bytes.
        payload: Vec<u8>,
    },

    /// Local node successfully subscribed to a `GossipSub` topic.
    GossipSubscribed {
        /// Topic name as requested by the application.
        topic: GossipTopic,
    },

    /// Local node failed to subscribe to a `GossipSub` topic.
    GossipSubscribeFailed {
        /// Topic name as requested by the application.
        topic: GossipTopic,
        /// Human-readable failure reason.
        reason: String,
    },
}
