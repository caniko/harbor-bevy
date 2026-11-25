//! Unopinionated Bevy replication substrate for marked ECS components and resources.
//!
//! Wire and chain-domain version bumps are hard network cutovers. Peers running
//! mixed incompatible versions must reject old transaction bytes loudly instead
//! of attempting best-effort decode or replay.
//!
//! # Cargo Features
//!
//! - `default`: empty.

extern crate self as bevy_replicated;

mod apply;
mod authority_ordered;
mod buffer;
mod chain;
pub mod dag;
pub mod dedup;
pub mod epoch;
pub mod fault;
pub mod interop;
pub mod mutation_log;
mod ordering;
mod plugin;
mod policy;
mod replicated_mut;
pub mod sync_barrier;
mod transaction;
mod wire;

/// Bevy-free wire-identity newtypes (seat IDs, session counters), re-exported
/// from the `veritasium_wire` crate so substrate consumers share one type
/// identity with non-Bevy binaries.
pub use veritasium_wire as wire_ids;

pub use dag::{DagHandle, DagResource};
pub use dedup::{KindIndex, KindSenderSeenSet, SeenSet};
pub use epoch::{EpochPolicy, LocalEpoch, SessionEpoch, try_accept_epoch};

pub use authority_ordered::{
    AuthorityOrdered, AuthorityOrderedEngine, AuthorityOrderedProposals, AuthorityOrderedRole,
    AuthorityPeer,
};
pub use bevy_replicated_macros::{Replicated, ReplicatedResource};
pub use buffer::{
    GapDetectionConfig, ReceiveTransactionStatus, TransactionBuffer, TransactionGapAlert,
    TransactionGapAlerts,
};
pub use chain::{
    CHAIN_DOMAIN_TAG, CHAIN_DOMAIN_VERSION, ChainHash, ChainHashCheckpoint, ChainHashConfig,
    ChainHashEnvelope, ChainHashOutbox, ChainHashReport, ChainHashReportStatus,
    ChainHistoryCapacity, DivergenceEvent, HashAlgorithm, HistoryEvicted, ReplicatedSet,
    ReplicationChain, format_hash_short, receive_chain_hash_report, request_chain_hash_report,
};
pub use mutation_log::{LogEntry, MutationLog, MutationLogSet};
pub use ordering::{
    CanonicalOrderingError, TransactionKey, canonical_transaction_cmp, canonicalize_transactions,
};
pub use plugin::{
    AppliedTransactions, ComponentRegistry, EntityReplicas, GossipOutbox, LiveReplicationFilter,
    RegisteredResource, ReplicatedAppExt, ReplicatedPlugin, ReplicatedResMut, ReplicationFatal,
    ReplicationFatalQueue, ResourceRegistry, TransactionEnvelope, commit_now, commit_now_as,
    insert_replicated_resource, insert_replicated_resource_as, receive_transaction,
    remove_replicated_resource, remove_replicated_resource_as, without_replication_capture,
};
pub use policy::{
    CanonicalEventual, CanonicalEventualEngine, ChannelKind, ConsistencyMarker, ConsistencyModel,
    OrderingMarker, OrderingPolicy, PassiveScheduler, ReliableOrdered, ReplicationEngine,
    ScheduleDriver,
};
pub use replicated_mut::{ReplicatedComponent, ReplicatedResource};
pub use sync_barrier::SyncBarrier;
pub use transaction::{
    ComponentId, Mutation, MutationOp, MutationTarget, NetworkId, OpaqueId, ReplicationIdentity,
    ReplicationParticipants, ReplicationPeerId, ResourceId, Transaction, WIRE_VERSION,
};
pub use wire::{
    TransactionWireId, WireError, decode_chain_hash_report, decode_transaction,
    encode_chain_hash_report, encode_transaction,
};

/// Result type returned by replicated ECS APIs.
pub type ReplicationResult<T, Id = OpaqueId> = Result<T, ReplicationError<Id>>;

/// Public error surface for replicated transaction capture, transport, and apply.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReplicationError<Id: NetworkId = OpaqueId> {
    /// A local Bevy entity was not bound to a stable network id.
    #[error("entity {0:?} has no replicated network identity")]
    MissingNetworkEntity(bevy::prelude::Entity),
    /// A transaction referenced a network id that has no local entity mapping.
    #[error("network entity has no local entity mapping")]
    MissingLocalEntity(Id),
    /// A transaction referenced a component id that was not registered.
    #[error("component id {0:?} is not registered")]
    UnknownComponent(ComponentId),
    /// A transaction referenced a resource id that was not registered.
    #[error("resource id {0:?} is not registered")]
    UnknownResource(ResourceId),
    /// Two different transactions reused the same `(peer_id, sequence)` key.
    #[error(
        "conflicting transaction for reused key {peer_id:?}:{sequence} (existing {existing_state} transaction has {existing_mutations} mutations, incoming has {incoming_mutations})"
    )]
    ConflictingTransactionKey {
        peer_id: ReplicationPeerId,
        sequence: u64,
        existing_state: &'static str,
        existing_mutations: usize,
        incoming_mutations: usize,
    },
    /// Canonical ordering rejected the transaction set.
    #[error("canonical ordering failed")]
    CanonicalOrdering(#[from] CanonicalOrderingError),
    /// Transaction or chain-hash wire encoding/decoding failed.
    #[error("wire encoding/decoding failed")]
    Wire(#[from] WireError),
    /// Vendored Bevy mediated component access failed.
    #[error("mediated ECS access failed")]
    MediatedAccess(#[from] bevy::ecs::query::QueryEntityError),
    /// A mediated capture could not be encoded because the component was not registered.
    #[error("component id {0:?} has no cached mediated post-image encoder")]
    MissingMediatedEncoder(ComponentId),
}

/// Stable app-facing imports for consumers that want the neutral substrate API.
pub mod prelude {
    pub use crate::{
        ComponentId, Mutation, MutationOp, MutationTarget, NetworkId, OpaqueId, RegisteredResource,
        Replicated, ReplicatedAppExt, ReplicatedComponent, ReplicatedPlugin, ReplicatedResMut,
        ReplicatedResource, ReplicationIdentity, ReplicationPeerId, ResourceId, ResourceRegistry,
        Transaction,
    };
}

#[cfg(test)]
#[path = "tests/core.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/phase06_tests.rs"]
mod phase06_tests;

#[cfg(test)]
#[path = "tests/phase07_tests.rs"]
mod phase07_tests;
