//! Commands sent to the network actor.

use std::fmt;
use std::ops::Deref;

use libp2p::{Multiaddr, PeerId};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GossipTopic(String);

impl GossipTopic {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl From<&str> for GossipTopic {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for GossipTopic {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl Deref for GossipTopic {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl fmt::Display for GossipTopic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for GossipTopic {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// Target for outgoing messages.
#[derive(Debug, Clone, Copy)]
pub enum SendTarget {
    /// Send to a specific connected peer.
    Peer(PeerId),
    /// Send to all connected peers.
    All,
}

/// Commands sent to the background network actor via the command channel.
#[derive(Debug)]
pub enum NetworkCommand {
    /// Start listening for incoming connections on the given address.
    Listen { addr: Multiaddr },

    /// Connect to a specific peer at a known address.
    Dial { peer_id: PeerId, addr: Multiaddr },

    /// Send a serialized message to one or all connected peers.
    SendMessage {
        /// Serialized `WireMessage` bytes.
        payload: Vec<u8>,
        /// Which peer(s) to send to.
        target: SendTarget,
    },

    /// Disconnect from a specific peer, or all peers if `None`.
    Disconnect { peer_id: Option<PeerId> },

    /// Send a graceful disconnect marker ("BYE") before closing.
    /// The remote peer can distinguish intentional disconnects from drops.
    GracefulDisconnect { peer_id: Option<PeerId> },

    /// Shut down the network actor and its libp2p swarm.
    Shutdown,

    /// Subscribe to a `GossipSub` topic.
    Subscribe { topic: GossipTopic },

    /// Unsubscribe from a `GossipSub` topic.
    Unsubscribe { topic: GossipTopic },

    /// Publish a message to a `GossipSub` topic.
    PublishGossip {
        topic: GossipTopic,
        payload: Vec<u8>,
    },
}
