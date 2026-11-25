use std::any::TypeId;

use bevy::prelude::*;

use crate::authority_ordered::{AuthorityOrdered, AuthorityOrderedProposals, AuthorityPeer};
use crate::buffer::{
    BufferedTransaction, GapDetectionConfig, TransactionBuffer, TransactionGapAlerts,
};
use crate::chain::{
    ReplicationChain, emit_due_chain_hash_report, process_pending_chain_hash_reports,
};
use crate::plugin::{
    AppliedTransactions, ComponentRegistry, EntityReplicas, ResourceRegistry,
    record_replication_fatal,
};
use crate::policy::ConsistencyModel;
use crate::wire::{TransactionWireId, encode_transaction};
use crate::{
    MutationTarget, ReplicationError, ReplicationIdentity, ReplicationParticipants,
    ReplicationResult,
};

pub fn apply_ready_transactions<Id: TransactionWireId, C: ConsistencyModel>(world: &mut World) {
    if let Err(error) = apply_ready_transactions_inner::<Id, C>(world) {
        record_replication_fatal(world, "bevy_replicated projection apply failed", error);
    }
}

pub(crate) fn apply_ready_transactions_inner<Id: TransactionWireId, C: ConsistencyModel>(
    world: &mut World,
) -> ReplicationResult<(), Id> {
    let participants = world.resource::<ReplicationParticipants>().clone();
    let transactions = if TypeId::of::<C>() == TypeId::of::<AuthorityOrdered>() {
        let authority_peer = world
            .get_resource::<AuthorityPeer>()
            .copied()
            .unwrap_or_default();
        let (authority_ordered, proposals) = world
            .resource_mut::<TransactionBuffer<Id>>()
            .drain_authority_ordered_transactions(authority_peer.peer_id());
        if !world.contains_resource::<AuthorityOrderedProposals<Id>>() {
            world.init_resource::<AuthorityOrderedProposals<Id>>();
        }
        for proposal in proposals {
            world
                .resource_mut::<AuthorityOrderedProposals<Id>>()
                .record(proposal);
        }
        authority_ordered
    } else {
        world
            .resource_mut::<TransactionBuffer<Id>>()
            .drain_ready_transactions(&participants)
    };
    for transaction in transactions {
        if C::admit(transaction.transaction.as_ref()) {
            apply_transaction::<Id>(world, transaction)?;
        }
    }
    emit_due_chain_hash_report(world)?;
    process_pending_chain_hash_reports(world);

    let timeout = world.resource::<GapDetectionConfig>().timeout;
    let alerts = world
        .resource_mut::<TransactionBuffer<Id>>()
        .detect_gap_alerts(timeout, &participants);
    if !alerts.is_empty() {
        world.resource_mut::<TransactionGapAlerts>().extend(alerts);
    }

    Ok(())
}

fn apply_transaction<Id: TransactionWireId>(
    world: &mut World,
    buffered: BufferedTransaction<Id>,
) -> ReplicationResult<(), Id> {
    let transaction = buffered.transaction;
    let wire_bytes = if buffered.wire_bytes.is_empty() {
        encode_transaction(transaction.as_ref())?
    } else {
        buffered.wire_bytes
    };
    // Local transactions are captured from mutations that already happened in
    // this world. Replaying their payloads is only a self-echo, and can overwrite
    // newer same-frame local state before the newer transaction is ready.
    let is_local_self_echo = world.resource::<ReplicationIdentity>().peer_id == transaction.peer_id;
    if !is_local_self_echo {
        for mutation in &transaction.mutations {
            match mutation.target() {
                MutationTarget::Component {
                    entity: network_id,
                    component_id,
                } => {
                    let entity = world
                        .resource::<EntityReplicas<Id>>()
                        .entity(network_id)
                        .ok_or(ReplicationError::MissingLocalEntity(network_id))?;
                    if world.get_entity(entity).is_err() {
                        world
                            .resource_mut::<EntityReplicas<Id>>()
                            .unbind_network_id(network_id);
                        if mutation.is_remove() {
                            log::debug!(
                                "skipping stale replicated remove for despawned entity network_id={network_id:?} component_id={component_id:?}",
                            );
                            continue;
                        }
                        return Err(ReplicationError::MissingLocalEntity(network_id));
                    }
                    world.resource_scope::<ComponentRegistry<Id>, _>(|world, registry| {
                        registry.apply(world, entity, mutation)
                    })?;
                }
                MutationTarget::Resource { .. } => {
                    world.resource_scope::<ResourceRegistry<Id>, _>(|world, registry| {
                        registry.apply(world, mutation)
                    })?;
                }
            }
        }
    }
    world
        .resource_mut::<ReplicationChain>()
        .fold_canonical_bytes(transaction.as_ref(), &wire_bytes);
    world
        .resource_mut::<AppliedTransactions<Id>>()
        .record(transaction);
    Ok(())
}
