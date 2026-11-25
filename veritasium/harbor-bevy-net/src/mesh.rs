use std::collections::HashMap;

use bevy::prelude::*;
use libp2p::{
    identity::{Keypair, ed25519},
    multiaddr::Protocol,
};

use crate::{
    Multiaddr, NetConfig, PeerId, RemoteSessionRef, SessionCleanupRequested, SessionRouter,
    SessionSendError,
};

type MeshSession = RemoteSessionRef<Box<dyn SessionRouter>>;

#[derive(Resource)]
pub struct SlotPeerMap {
    capacity: u8,
    pub local_slot: u8,
    pub coordinator_peer: Option<PeerId>,
    slots: [Option<PeerId>; 8],
    pub sessions: HashMap<PeerId, MeshSession>,
    pub pairs: HashMap<u8, u8>,
    pub names: HashMap<PeerId, String>,
    pub result_public_keys: HashMap<PeerId, [u8; 32]>,
    pub listen_ports: HashMap<PeerId, u16>,
    pub coordinator_nonce: Option<[u8; 32]>,
    pub joiner_nonces: HashMap<PeerId, [u8; 32]>,
}

impl Default for SlotPeerMap {
    fn default() -> Self {
        Self::new(0, 8)
    }
}

impl SlotPeerMap {
    pub fn new(local_slot: u8, capacity: u8) -> Self {
        let capacity = if (2..=8).contains(&capacity) {
            capacity
        } else {
            8
        };
        Self {
            capacity,
            local_slot: if local_slot < capacity { local_slot } else { 0 },
            coordinator_peer: None,
            slots: [None; 8],
            sessions: HashMap::new(),
            pairs: HashMap::new(),
            names: HashMap::new(),
            result_public_keys: HashMap::new(),
            listen_ports: HashMap::new(),
            coordinator_nonce: None,
            joiner_nonces: HashMap::new(),
        }
    }

    pub fn capacity(&self) -> u8 {
        self.capacity
    }

    pub fn allocate(&mut self, peer: PeerId) -> Option<u8> {
        if let Some(assigned) = self.slot_for(&peer) {
            return Some(assigned);
        }
        let slot = (0..self.capacity)
            .find(|&slot| slot != self.local_slot && self.peer_for(slot).is_none())?;
        self.slots[usize::from(slot)] = Some(peer);
        Some(slot)
    }

    /// Record a slot assignment learned from the membership protocol without
    /// replacing an existing binding or mapping a peer to two slots.
    pub fn bind(&mut self, slot: u8, peer: PeerId) -> bool {
        if slot >= self.capacity || slot == self.local_slot {
            return false;
        }
        if let Some(existing) = self.slot_for(&peer) {
            return existing == slot;
        }
        let mapped = &mut self.slots[usize::from(slot)];
        if mapped.is_some() {
            return false;
        }
        *mapped = Some(peer);
        true
    }

    pub fn peer_for(&self, slot: u8) -> Option<PeerId> {
        self.slots.get(usize::from(slot)).copied().flatten()
    }

    pub fn slot_for(&self, peer: &PeerId) -> Option<u8> {
        (0..self.capacity).find(|&slot| self.peer_for(slot).as_ref() == Some(peer))
    }

    pub fn session_for(&self, slot: u8) -> Result<&MeshSession, SlotSendError> {
        let peer = self
            .peer_for(slot)
            .ok_or(SlotSendError::UnknownSlot(slot))?;
        self.sessions
            .get(&peer)
            .ok_or(SlotSendError::MissingSession(peer))
    }

    pub fn send(
        &self,
        slot: u8,
        config: &NetConfig,
        signer: &Keypair,
        bytes: Vec<u8>,
    ) -> Result<(), SlotSendError> {
        self.session_for(slot)?
            .send(config, signer, bytes)
            .map_err(|error| match error {
                SessionSendError::Full => SlotSendError::Full,
                SessionSendError::Closed => SlotSendError::Closed,
                error => SlotSendError::Transport(error.to_string()),
            })
    }

    /// An observed UDP address can hold an ephemeral source port rather than a
    /// listener port. Replace it only with the known port for this peer.
    pub fn dial_addr(&self, peer: &PeerId, observed: &Multiaddr) -> Option<Multiaddr> {
        let mut parts = observed.iter();
        if matches!(parts.next(), Some(Protocol::Ip4(_) | Protocol::Ip6(_)))
            && matches!(parts.next(), Some(Protocol::Udp(_)))
            && !parts.any(|part| matches!(part, Protocol::P2pCircuit))
        {
            let port = *self.listen_ports.get(peer)?;
            return (port != 0)
                .then(|| observed.replace(1, |_| Some(Protocol::Udp(port))))
                .flatten();
        }
        Some(observed.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SlotSendError {
    #[error("Unknown slot {0}")]
    UnknownSlot(u8),
    #[error("No session for peer {0}")]
    MissingSession(PeerId),
    #[error("Reliable outbound window or command mailbox is full")]
    Full,
    #[error("Session transport is closed")]
    Closed,
    #[error("Could not send over transport: {0}")]
    Transport(String),
}

#[derive(
    Debug,
    Default,
    Clone,
    PartialEq,
    Eq,
    Resource,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct BlockedPeerBytes {
    pub ids: Vec<Vec<u8>>,
    pub platform_ids: Vec<u64>,
}

impl BlockedPeerBytes {
    pub fn contains_peer(&self, peer: &PeerId) -> bool {
        self.ids.contains(&peer.to_bytes())
    }
}

/// No key travels in the envelope: the verifier derives it from an independently
/// known Ed25519 peer identity. The caller supplies the same canonical encoding
/// on both sides and a purpose unique to the message type and protocol version.
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct SignedControlEnvelope<T> {
    pub payload: T,
    pub signature: [u8; 64],
}

impl<T> SignedControlEnvelope<T> {
    pub fn sign(
        payload: T,
        signer: &Keypair,
        config: &NetConfig,
        purpose: &str,
        canonical: impl FnOnce(&T) -> Vec<u8>,
    ) -> Option<Self> {
        let key = signer.clone().try_into_ed25519().ok()?;
        let signature = key
            .sign(&control_bytes(config, purpose, canonical(&payload)))
            .try_into()
            .ok()?;
        Some(Self { payload, signature })
    }

    pub fn verify(
        &self,
        expected: &PeerId,
        config: &NetConfig,
        purpose: &str,
        canonical: impl FnOnce(&T) -> Vec<u8>,
    ) -> bool {
        thespan::ed25519_public_key_from_peer_id(expected)
            .and_then(|bytes| ed25519::PublicKey::try_from_bytes(&bytes).ok())
            .is_some_and(|key| {
                key.verify(
                    &control_bytes(config, purpose, canonical(&self.payload)),
                    &self.signature,
                )
            })
    }
}

fn control_bytes(config: &NetConfig, purpose: &str, canonical: Vec<u8>) -> Vec<u8> {
    let mut bytes = config.control_domain(purpose).into_bytes();
    bytes.extend(canonical);
    bytes
}

pub struct MeshPlugin;

impl Plugin for MeshPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SlotPeerMap>()
            .init_resource::<BlockedPeerBytes>()
            .add_observer(
                |_: On<SessionCleanupRequested>, mut map: ResMut<SlotPeerMap>| {
                    *map = SlotPeerMap::new(map.local_slot, map.capacity);
                },
            );
    }
}
