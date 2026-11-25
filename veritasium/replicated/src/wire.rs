use crate::transaction::MutationOp;
use crate::{
    ChainHashReport, ComponentId, Mutation, MutationTarget, NetworkId, ReplicationPeerId,
    ResourceId, Transaction, WIRE_VERSION,
};

/// Network id types that can serialize replicated transactions.
///
/// This trait is blanket-implemented for `NetworkId` types whose archived
/// transaction layout satisfies rkyv's serialize/validate bounds. It exists to
/// keep those bounds off every public call site while still making the wire
/// requirement explicit at transport boundaries.
pub trait TransactionWireId: NetworkId {
    /// Encode a transaction into canonical transport bytes.
    fn encode_transaction(transaction: &Transaction<Self>) -> Result<Vec<u8>, WireError>;

    /// Decode canonical transport bytes into a transaction.
    fn decode_transaction(bytes: &[u8]) -> Result<Transaction<Self>, WireError>;
}

impl<Id> TransactionWireId for Id
where
    Id: NetworkId,
    WireTransaction<Id>: for<'a> rkyv::Serialize<
            rkyv::api::high::HighSerializer<
                rkyv::util::AlignedVec,
                rkyv::ser::allocator::ArenaHandle<'a>,
                rkyv::rancor::Error,
            >,
        >,
    <WireTransaction<Id> as rkyv::Archive>::Archived: for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>
        + rkyv::Deserialize<
            WireTransaction<Id>,
            rkyv::api::high::HighDeserializer<rkyv::rancor::Error>,
        >,
{
    fn encode_transaction(transaction: &Transaction<Self>) -> Result<Vec<u8>, WireError> {
        let wire = WireTransaction::from_transaction(transaction);
        rkyv::to_bytes::<rkyv::rancor::Error>(&wire)
            .map(|bytes| bytes.to_vec())
            .map_err(WireError::Encode)
    }

    fn decode_transaction(bytes: &[u8]) -> Result<Transaction<Self>, WireError> {
        let transaction = rkyv::from_bytes::<WireTransaction<Self>, rkyv::rancor::Error>(bytes)
            .map_err(WireError::Decode)?;
        if transaction.version != WIRE_VERSION {
            return Err(WireError::Version {
                expected: WIRE_VERSION,
                found: transaction.version,
            });
        }
        Ok(transaction.into_transaction())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
enum WireMutation<Id: NetworkId> {
    ComponentInsert {
        entity: Id,
        component_id: ComponentId,
        payload: Vec<u8>,
    },
    ComponentModify {
        entity: Id,
        component_id: ComponentId,
        payload: Vec<u8>,
    },
    ComponentRemove {
        entity: Id,
        component_id: ComponentId,
    },
    ResourceInsert {
        resource_id: ResourceId,
        payload: Vec<u8>,
    },
    ResourceModify {
        resource_id: ResourceId,
        payload: Vec<u8>,
    },
    ResourceRemove {
        resource_id: ResourceId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct WireTransaction<Id: NetworkId> {
    version: u8,
    peer_id: ReplicationPeerId,
    sequence: u64,
    mutations: Vec<WireMutation<Id>>,
}

impl<Id: NetworkId> WireTransaction<Id> {
    fn from_transaction(transaction: &Transaction<Id>) -> Self {
        Self {
            version: transaction.version,
            peer_id: transaction.peer_id,
            sequence: transaction.sequence,
            mutations: transaction
                .mutations
                .iter()
                .map(WireMutation::from_mutation)
                .collect(),
        }
    }

    fn into_transaction(self) -> Transaction<Id> {
        Transaction {
            version: self.version,
            peer_id: self.peer_id,
            sequence: self.sequence,
            mutations: self
                .mutations
                .into_iter()
                .map(WireMutation::into_mutation)
                .collect(),
        }
    }
}

impl<Id: NetworkId> WireMutation<Id> {
    fn from_mutation(mutation: &Mutation<Id>) -> Self {
        match (mutation.target(), &mutation.payload) {
            (
                MutationTarget::Component {
                    entity,
                    component_id,
                },
                MutationOp::Insert(payload),
            ) => Self::ComponentInsert {
                entity,
                component_id,
                payload: payload.clone(),
            },
            (
                MutationTarget::Component {
                    entity,
                    component_id,
                },
                MutationOp::Modify(payload),
            ) => Self::ComponentModify {
                entity,
                component_id,
                payload: payload.clone(),
            },
            (
                MutationTarget::Component {
                    entity,
                    component_id,
                },
                MutationOp::Remove,
            ) => Self::ComponentRemove {
                entity,
                component_id,
            },
            (MutationTarget::Resource { resource_id }, MutationOp::Insert(payload)) => {
                Self::ResourceInsert {
                    resource_id,
                    payload: payload.clone(),
                }
            }
            (MutationTarget::Resource { resource_id }, MutationOp::Modify(payload)) => {
                Self::ResourceModify {
                    resource_id,
                    payload: payload.clone(),
                }
            }
            (MutationTarget::Resource { resource_id }, MutationOp::Remove) => {
                Self::ResourceRemove { resource_id }
            }
        }
    }

    fn into_mutation(self) -> Mutation<Id> {
        match self {
            Self::ComponentInsert {
                entity,
                component_id,
                payload,
            } => Mutation::insert(entity, component_id, payload),
            Self::ComponentModify {
                entity,
                component_id,
                payload,
            } => Mutation::modify(entity, component_id, payload),
            Self::ComponentRemove {
                entity,
                component_id,
            } => Mutation::remove(entity, component_id),
            Self::ResourceInsert {
                resource_id,
                payload,
            } => Mutation::insert_resource(resource_id, payload),
            Self::ResourceModify {
                resource_id,
                payload,
            } => Mutation::modify_resource(resource_id, payload),
            Self::ResourceRemove { resource_id } => Mutation::remove_resource(resource_id),
        }
    }
}

/// Errors while encoding or decoding replication wire payloads.
///
/// Version mismatches are protocol cutovers: callers should reject the payload
/// loudly rather than attempting compatibility shims.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// rkyv serialization failed while producing transport bytes.
    #[error("rkyv encode failed")]
    Encode(#[from] rkyv::rancor::Error),
    /// rkyv validation or deserialization failed while reading transport bytes.
    #[error("rkyv decode failed")]
    Decode(#[source] rkyv::rancor::Error),
    /// The encoded transaction was produced by an incompatible wire version.
    #[error("unsupported replication wire version {found}; expected {expected}")]
    Version { expected: u8, found: u8 },
    /// An internal replication invariant was violated before encoding.
    #[error("replication invariant broken: {detail}")]
    InvariantBroken { detail: String },
}

/// Encode a transaction into stable transport bytes.
///
/// # Errors
///
/// Returns [`WireError::Encode`] if rkyv cannot serialize the archived
/// transaction layout.
///
/// # Examples
///
/// ```
/// use bevy_replicated::{
///     ComponentId, Mutation, OpaqueId, ReplicationPeerId, Transaction, encode_transaction,
/// };
///
/// let transaction = Transaction::new(
///     ReplicationPeerId([1; 32]),
///     0,
///     vec![Mutation::modify(OpaqueId(1), ComponentId(7), vec![42])],
/// );
///
/// let bytes = encode_transaction(&transaction).expect("transaction encodes");
/// assert!(!bytes.is_empty());
/// ```
pub fn encode_transaction<Id: TransactionWireId>(
    transaction: &Transaction<Id>,
) -> Result<Vec<u8>, WireError> {
    Id::encode_transaction(transaction)
}

/// Decode transport bytes into a transaction.
///
/// # Errors
///
/// Returns [`WireError::Decode`] for invalid archived bytes and
/// [`WireError::Version`] when the transaction's wire version does not match
/// this crate.
///
/// # Examples
///
/// ```
/// use bevy_replicated::{
///     ComponentId, Mutation, OpaqueId, ReplicationPeerId, Transaction, decode_transaction,
///     encode_transaction,
/// };
///
/// let transaction = Transaction::new(
///     ReplicationPeerId([1; 32]),
///     0,
///     vec![Mutation::remove(OpaqueId(1), ComponentId(7))],
/// );
/// let bytes = encode_transaction(&transaction).expect("transaction encodes");
///
/// assert_eq!(decode_transaction::<OpaqueId>(&bytes).unwrap(), transaction);
/// ```
pub fn decode_transaction<Id: TransactionWireId>(
    bytes: &[u8],
) -> Result<Transaction<Id>, WireError> {
    Id::decode_transaction(bytes)
}

/// Encode a chain-hash report into transport bytes.
///
/// # Errors
///
/// Returns [`WireError::Encode`] if rkyv cannot serialize the report.
pub fn encode_chain_hash_report(report: &ChainHashReport) -> Result<Vec<u8>, WireError> {
    rkyv::to_bytes::<rkyv::rancor::Error>(report)
        .map(|bytes| bytes.to_vec())
        .map_err(WireError::Encode)
}

/// Decode chain-hash report transport bytes.
///
/// # Errors
///
/// Returns [`WireError::Decode`] if the bytes are not a valid chain-hash report.
pub fn decode_chain_hash_report(bytes: &[u8]) -> Result<ChainHashReport, WireError> {
    rkyv::from_bytes::<ChainHashReport, rkyv::rancor::Error>(bytes).map_err(WireError::Decode)
}
