// intentionally oversize: release-gate baseline; split requires scoped semantic extraction.
use std::collections::BTreeSet;

use bevy::prelude::Resource;

pub const WIRE_VERSION: u8 = 3;

/// Stable peer identifier used in transaction keys and chain hashes.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
pub struct ReplicationPeerId(pub [u8; 32]);

impl ReplicationPeerId {
    /// Derive a deterministic peer id from domain-separated stable bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use bevy_replicated::ReplicationPeerId;
    ///
    /// let first = ReplicationPeerId::from_stable_bytes(b"lobby", b"peer-a");
    /// let second = ReplicationPeerId::from_stable_bytes(b"lobby", b"peer-a");
    ///
    /// assert_eq!(first, second);
    /// ```
    pub fn from_stable_bytes(domain: &[u8], bytes: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"bevy_replicated.peer_id.v1");
        hasher.update(&(domain.len() as u64).to_le_bytes());
        hasher.update(domain);
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
        Self(hasher.finalize().into())
    }
}

/// Stable numeric identifier for a replicated component type.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
pub struct ComponentId(pub u32);

/// Stable numeric identifier for a replicated singleton resource type.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
pub struct ResourceId(pub u32);

/// Network-stable identity for replicated entities.
///
/// Implementors are part of the byte identity folded into transaction wire
/// bytes and the replication chain hash. Archived layouts must be portable
/// across supported peers; this crate pins rkyv to little-endian archives, but
/// consumer ids must still avoid platform-dependent layouts. `Ord` must be a
/// total, platform-stable order. It is provided for consumer maps and payloads;
/// transaction sequencing itself is ordered only by `(sequence, peer_id)`.
///
/// # Examples
///
/// ```
/// use bevy_replicated::{NetworkId, OpaqueId};
///
/// fn accepts_network_id<Id: NetworkId>(id: Id) -> Id {
///     id
/// }
///
/// assert_eq!(accepts_network_id(OpaqueId(7)), OpaqueId(7));
/// ```
pub trait NetworkId:
    rkyv::Archive + Copy + Eq + Ord + core::hash::Hash + core::fmt::Debug + Send + Sync + 'static
{
}

/// Default opaque entity id for applications that do not need a custom id type.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
pub struct OpaqueId(pub u64);

impl OpaqueId {
    /// Creates an opaque network id for examples, tests, and applications that
    /// already have a stable numeric entity key.
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Returns the wrapped numeric id.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl NetworkId for OpaqueId {}

/// Target affected by a replicated mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum MutationTarget<Id: NetworkId = OpaqueId> {
    /// Mutation of one component on a network-identified entity.
    Component {
        entity: Id,
        component_id: ComponentId,
    },
    /// Mutation of a singleton resource identified by a stable resource id.
    Resource { resource_id: ResourceId },
}

/// One component or resource mutation captured for replication.
///
/// # Examples
///
/// ```
/// use bevy_replicated::{ComponentId, Mutation, MutationOp, OpaqueId};
///
/// let mutation = Mutation::modify(OpaqueId(1), ComponentId(7), vec![1, 2, 3]);
///
/// assert_eq!(mutation.entity(), OpaqueId(1));
/// assert!(matches!(mutation.payload(), MutationOp::Modify(bytes) if bytes == &[1, 2, 3]));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct Mutation<Id: NetworkId = OpaqueId> {
    pub(crate) target: MutationTarget<Id>,
    pub(crate) payload: MutationOp,
}

/// Operation carried by a [`Mutation`].
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub enum MutationOp {
    Insert(Vec<u8>),
    Modify(Vec<u8>),
    Remove,
}

impl<Id: NetworkId> Mutation<Id> {
    /// Create a component insertion mutation.
    pub fn insert(entity: Id, component_id: ComponentId, payload: Vec<u8>) -> Self {
        Self {
            target: MutationTarget::Component {
                entity,
                component_id,
            },
            payload: MutationOp::Insert(payload),
        }
    }

    /// Create a component modification mutation.
    pub fn modify(entity: Id, component_id: ComponentId, payload: Vec<u8>) -> Self {
        Self {
            target: MutationTarget::Component {
                entity,
                component_id,
            },
            payload: MutationOp::Modify(payload),
        }
    }

    /// Create a component removal mutation.
    pub fn remove(entity: Id, component_id: ComponentId) -> Self {
        Self {
            target: MutationTarget::Component {
                entity,
                component_id,
            },
            payload: MutationOp::Remove,
        }
    }

    /// Create a resource insertion mutation.
    pub fn insert_resource(resource_id: ResourceId, payload: Vec<u8>) -> Self {
        Self {
            target: MutationTarget::Resource { resource_id },
            payload: MutationOp::Insert(payload),
        }
    }

    /// Create a resource modification mutation.
    pub fn modify_resource(resource_id: ResourceId, payload: Vec<u8>) -> Self {
        Self {
            target: MutationTarget::Resource { resource_id },
            payload: MutationOp::Modify(payload),
        }
    }

    /// Create a resource removal mutation.
    pub fn remove_resource(resource_id: ResourceId) -> Self {
        Self {
            target: MutationTarget::Resource { resource_id },
            payload: MutationOp::Remove,
        }
    }

    /// Return the target affected by this mutation.
    pub fn target(&self) -> MutationTarget<Id> {
        self.target
    }

    /// Return the network entity id affected by this mutation.
    ///
    /// Panics if this mutation targets a singleton resource. Prefer
    /// [`component_target`](Self::component_target) for target-agnostic code.
    pub fn entity(&self) -> Id {
        match self.target {
            MutationTarget::Component { entity, .. } => entity,
            MutationTarget::Resource { resource_id } => {
                panic!("resource mutation {resource_id:?} has no entity id")
            }
        }
    }

    /// Return the stable component id affected by this mutation.
    ///
    /// Panics if this mutation targets a singleton resource. Prefer
    /// [`component_target`](Self::component_target) for target-agnostic code.
    pub fn component_id(&self) -> ComponentId {
        match self.target {
            MutationTarget::Component { component_id, .. } => component_id,
            MutationTarget::Resource { resource_id } => {
                panic!("resource mutation {resource_id:?} has no component id")
            }
        }
    }

    /// Return the component target, if this mutation targets a component.
    pub fn component_target(&self) -> Option<(Id, ComponentId)> {
        match self.target {
            MutationTarget::Component {
                entity,
                component_id,
            } => Some((entity, component_id)),
            MutationTarget::Resource { .. } => None,
        }
    }

    /// Return the resource id, if this mutation targets a resource.
    pub fn resource_id(&self) -> Option<ResourceId> {
        match self.target {
            MutationTarget::Component { .. } => None,
            MutationTarget::Resource { resource_id } => Some(resource_id),
        }
    }

    /// Return whether this mutation targets a component.
    pub fn is_component_target(&self) -> bool {
        matches!(self.target, MutationTarget::Component { .. })
    }

    /// Return whether this mutation targets a singleton resource.
    pub fn is_resource_target(&self) -> bool {
        matches!(self.target, MutationTarget::Resource { .. })
    }

    /// Return the mutation operation and payload.
    pub fn payload(&self) -> &MutationOp {
        &self.payload
    }

    /// Return inserted or modified component bytes, or `None` for removals.
    pub fn payload_bytes(&self) -> Option<&[u8]> {
        match &self.payload {
            MutationOp::Insert(payload) | MutationOp::Modify(payload) => Some(payload),
            MutationOp::Remove => None,
        }
    }

    /// Return whether this mutation inserts a component.
    pub fn is_insert(&self) -> bool {
        matches!(self.payload, MutationOp::Insert(_))
    }

    /// Return whether this mutation modifies a component.
    pub fn is_modify(&self) -> bool {
        matches!(self.payload, MutationOp::Modify(_))
    }

    /// Return whether this mutation removes a component.
    pub fn is_remove(&self) -> bool {
        matches!(self.payload, MutationOp::Remove)
    }
}

/// Ordered batch of mutations produced by one peer.
///
/// # Examples
///
/// ```
/// use bevy_replicated::{
///     ComponentId, Mutation, OpaqueId, ReplicationPeerId, Transaction, WIRE_VERSION,
/// };
///
/// let transaction = Transaction::new(
///     ReplicationPeerId([1; 32]),
///     3,
///     vec![Mutation::remove(OpaqueId(9), ComponentId(4))],
/// );
///
/// assert_eq!(transaction.version, WIRE_VERSION);
/// assert_eq!(transaction.sequence, 3);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct Transaction<Id: NetworkId = OpaqueId> {
    pub version: u8,
    pub peer_id: ReplicationPeerId,
    pub sequence: u64,
    pub mutations: Vec<Mutation<Id>>,
}

impl<Id: NetworkId> Transaction<Id> {
    /// Create a transaction at the current replication wire version.
    pub fn new(peer_id: ReplicationPeerId, sequence: u64, mutations: Vec<Mutation<Id>>) -> Self {
        Self {
            version: WIRE_VERSION,
            peer_id,
            sequence,
            mutations,
        }
    }
}

/// Local identity used when producing outbound replicated transactions.
#[derive(Resource, Debug, Clone)]
pub struct ReplicationIdentity {
    pub peer_id: ReplicationPeerId,
}

impl Default for ReplicationIdentity {
    fn default() -> Self {
        Self {
            peer_id: ReplicationPeerId([0; 32]),
        }
    }
}

#[cfg(test)]
thread_local! {
    static REPLICATION_PARTICIPANTS_CLONE_COUNT: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_replication_participants_clone_count() {
    REPLICATION_PARTICIPANTS_CLONE_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn replication_participants_clone_count() -> u64 {
    REPLICATION_PARTICIPANTS_CLONE_COUNT.with(std::cell::Cell::get)
}

/// Set of peers considered during canonical transaction ordering.
#[derive(Resource, Debug, PartialEq, Eq)]
pub struct ReplicationParticipants {
    peers: BTreeSet<ReplicationPeerId>,
    dynamic_discovery: bool,
}

impl Clone for ReplicationParticipants {
    fn clone(&self) -> Self {
        #[cfg(test)]
        REPLICATION_PARTICIPANTS_CLONE_COUNT.with(|count| count.set(count.get() + 1));
        Self {
            peers: self.peers.clone(),
            dynamic_discovery: self.dynamic_discovery,
        }
    }
}

impl Default for ReplicationParticipants {
    fn default() -> Self {
        Self {
            peers: BTreeSet::new(),
            dynamic_discovery: true,
        }
    }
}

impl ReplicationParticipants {
    /// Replace participants with an explicit static peer set.
    pub fn set<I>(&mut self, peers: I)
    where
        I: IntoIterator<Item = ReplicationPeerId>,
    {
        self.peers = peers.into_iter().collect();
        self.dynamic_discovery = false;
    }

    /// Return to dynamic peer discovery based on observed transactions.
    pub fn set_dynamic_discovery(&mut self) {
        self.peers.clear();
        self.dynamic_discovery = true;
    }

    /// Record a peer if dynamic discovery is enabled.
    pub fn observe(&mut self, peer_id: ReplicationPeerId) {
        if self.dynamic_discovery {
            self.peers.insert(peer_id);
        }
    }

    /// Iterate peers currently known to the replication engine.
    pub fn peers(&self) -> impl Iterator<Item = ReplicationPeerId> + '_ {
        self.peers.iter().copied()
    }

    /// Return the number of known peers.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Return whether the known peer set is empty.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Return whether peers are discovered from observed transactions.
    pub fn is_dynamic_discovery(&self) -> bool {
        self.dynamic_discovery
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::decode_transaction;

    const RECORDED_TRANSACTION_V1: &[u8] = &[
        1, 2, 3, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 228,
        255, 255, 255, 3, 0, 0, 0, 0, 0, 0, 0, 1, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
        5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0,
        0, 176, 255, 255, 255, 1, 0, 0, 0,
    ];
    const RECORDED_TRANSACTION_V2: &[u8] = &[
        1, 2, 3, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 228,
        255, 255, 255, 3, 0, 0, 0, 0, 0, 0, 0, 2, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
        5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0,
        0, 176, 255, 255, 255, 1, 0, 0, 0,
    ];

    #[test]
    fn mutation_struct_field_access() {
        let mutation = Mutation {
            target: MutationTarget::Component {
                entity: OpaqueId(1),
                component_id: ComponentId(7),
            },
            payload: MutationOp::Insert(vec![1, 2, 3]),
        };

        assert_eq!(mutation.entity(), OpaqueId(1));
        assert_eq!(mutation.component_id(), ComponentId(7));
        assert_eq!(mutation.payload, MutationOp::Insert(vec![1, 2, 3]));
    }

    #[test]
    fn mutation_op_round_trip() {
        for op in [
            MutationOp::Insert(vec![1, 2, 3]),
            MutationOp::Modify(vec![4, 5, 6]),
            MutationOp::Remove,
        ] {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&op).expect("mutation op encodes");
            let decoded =
                rkyv::from_bytes::<MutationOp, rkyv::rancor::Error>(&bytes).expect("op decodes");

            assert_eq!(decoded, op);
        }
    }

    #[test]
    fn mutation_on_wire_byte_compat_with_pre_normalization() {
        let result = decode_transaction::<OpaqueId>(RECORDED_TRANSACTION_V1);
        assert!(
            matches!(
                result,
                Err(crate::wire::WireError::Decode(_))
                    | Err(crate::wire::WireError::Version {
                        expected: WIRE_VERSION,
                        found: 1
                    })
            ),
            "recorded v1 transaction must be loudly rejected after the v2 cutover: {result:?}"
        );
    }

    #[test]
    fn v2_transaction_bytes_rejected_after_resource_target_cutover() {
        let result = decode_transaction::<OpaqueId>(RECORDED_TRANSACTION_V2);
        assert!(
            matches!(
                result,
                Err(crate::wire::WireError::Decode(_))
                    | Err(crate::wire::WireError::Version {
                        expected: WIRE_VERSION,
                        found: 2
                    })
            ),
            "recorded v2 transaction must be loudly rejected after the v3 resource-target cutover: {result:?}"
        );
    }
}
