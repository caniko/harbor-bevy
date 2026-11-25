// intentionally oversize: release-gate baseline; split requires scoped semantic extraction.
use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::Arc;

use crate::authority_ordered::{
    AuthorityOrdered, AuthorityOrderedProposals, AuthorityOrderedRole, AuthorityPeer,
};
use bevy::ecs::change_detection::Tick;
use bevy::ecs::component::{ComponentId as EngineComponentId, MediatedAccess, MediatedCapture};
use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::SystemParam;
use bevy::ecs::world::DeferredWorld;
use bevy::prelude::*;
use bevy::state::state::StateTransitionSystems;

use crate::apply::{apply_ready_transactions, apply_ready_transactions_inner};
use crate::buffer::{
    GapDetectionConfig, ReceiveTransactionStatus, TransactionBuffer, TransactionGapAlerts,
};
use crate::chain::{
    ChainHashCheckpoint, ChainHashConfig, ChainHashExchangeState, ChainHashOutbox, ChainHashReport,
    ChainHistoryCapacity, DivergenceEvent, RemoteChainHashReports, ReplicatedSet, ReplicationChain,
    process_chain_hash_checkpoints, push_report,
};
use crate::policy::{
    CanonicalEventual, ConsistencyModel, OrderingPolicy, ReliableOrdered, ReplicationEngine,
    ScheduleDriver,
};
use crate::replicated_mut::{ReplicatedComponent, ReplicatedResource};
use crate::sync_barrier::SyncBarrier;
use crate::transaction::{ReplicationIdentity, ReplicationParticipants};
use crate::wire::{TransactionWireId, encode_transaction};
use crate::{
    ComponentId, Mutation, NetworkId, OpaqueId, ReplicationError, ReplicationResult, ResourceId,
    Transaction,
};

/// Runtime policy for mediated live-mutation capture.
///
/// Frameworks can register every component needed for snapshots and rejoin while
/// suppressing selected in-place post-images from the live transaction stream.
/// This is intended for components whose live ordering is owned by a separate
/// domain protocol, but which still need stable component IDs and apply support.
#[derive(Resource, Debug, Clone, Default)]
pub struct LiveReplicationFilter {
    suppressed_modify_components: HashSet<ComponentId>,
}

impl LiveReplicationFilter {
    /// Suppress mediated `Modify` mutations for a stable component id.
    pub fn suppress_modify_component(&mut self, component_id: ComponentId) {
        self.suppressed_modify_components.insert(component_id);
    }

    /// Replace the suppressed mediated `Modify` component set.
    pub fn set_suppressed_modify_components(
        &mut self,
        component_ids: impl IntoIterator<Item = ComponentId>,
    ) {
        self.suppressed_modify_components = component_ids.into_iter().collect();
    }

    /// Clear all live-mutation suppression.
    pub fn clear(&mut self) {
        self.suppressed_modify_components.clear();
    }

    /// Return whether mediated `Modify` capture is suppressed for `component_id`.
    pub fn suppresses_modify_component(&self, component_id: ComponentId) -> bool {
        self.suppressed_modify_components.contains(&component_id)
    }
}

#[derive(Resource, Debug)]
pub struct EntityReplicas<Id: NetworkId = OpaqueId> {
    local_to_network: HashMap<Entity, Id>,
    network_to_local: HashMap<Id, Entity>,
    /// Change tick at which each entity was first bound (its replication baseline).
    ///
    /// A structural insert whose component was added at or before this tick is part
    /// of the entity's deterministic birth (the spawn bundle, or pre-state set up in
    /// the same tick) and must NOT be recorded as a delta — both peers reconstruct
    /// it by spawning identically. Only structural changes strictly after the
    /// baseline are genuine deltas. See [`insert_hook`].
    baseline_tick: HashMap<Entity, Tick>,
}

impl<Id: NetworkId> Default for EntityReplicas<Id> {
    fn default() -> Self {
        Self {
            local_to_network: HashMap::new(),
            network_to_local: HashMap::new(),
            baseline_tick: HashMap::new(),
        }
    }
}

impl<Id: NetworkId> EntityReplicas<Id> {
    pub fn bind(&mut self, entity: Entity, network_id: Id) {
        if let Some(previous_network_id) = self.local_to_network.insert(entity, network_id)
            && previous_network_id != network_id
            && self.network_to_local.get(&previous_network_id) == Some(&entity)
        {
            self.network_to_local.remove(&previous_network_id);
        }
        if let Some(previous_entity) = self.network_to_local.insert(network_id, entity)
            && previous_entity != entity
            && self.local_to_network.get(&previous_entity) == Some(&network_id)
        {
            self.local_to_network.remove(&previous_entity);
        }
    }

    /// Records `entity`'s replication baseline tick (the tick it was first bound).
    ///
    /// Called by the spawn-binding observers. Keeps the earliest baseline if called
    /// more than once for the same entity (re-binds on move must not reset it).
    pub fn mark_baseline(&mut self, entity: Entity, tick: Tick) {
        self.baseline_tick.entry(entity).or_insert(tick);
    }

    /// The tick at which `entity` was first bound, if known.
    pub fn baseline_tick(&self, entity: Entity) -> Option<Tick> {
        self.baseline_tick.get(&entity).copied()
    }

    pub fn unbind_entity(&mut self, entity: Entity) -> Option<Id> {
        let network_id = self.local_to_network.remove(&entity)?;
        if self.network_to_local.get(&network_id) == Some(&entity) {
            self.network_to_local.remove(&network_id);
        }
        self.baseline_tick.remove(&entity);
        Some(network_id)
    }

    pub fn unbind_network_id(&mut self, network_id: Id) -> Option<Entity> {
        let entity = self.network_to_local.remove(&network_id)?;
        if self.local_to_network.get(&entity) == Some(&network_id) {
            self.local_to_network.remove(&entity);
        }
        self.baseline_tick.remove(&entity);
        Some(entity)
    }

    pub fn unbind_network_id_keep_entity(&mut self, network_id: Id) -> Option<Entity> {
        self.network_to_local.remove(&network_id)
    }

    pub fn unbind_network_ids_where(&mut self, mut predicate: impl FnMut(Id) -> bool) {
        let network_ids: Vec<_> = self
            .network_to_local
            .keys()
            .copied()
            .filter(|network_id| predicate(*network_id))
            .collect();
        for network_id in network_ids {
            self.unbind_network_id(network_id);
        }
    }

    pub fn network_id(&self, entity: Entity) -> Option<Id> {
        self.local_to_network.get(&entity).copied()
    }

    pub fn entity(&self, network_id: Id) -> Option<Entity> {
        self.network_to_local.get(&network_id).copied()
    }
}

#[derive(Debug, Clone)]
pub struct TransactionEnvelope<Id: NetworkId = OpaqueId> {
    pub transaction: Arc<Transaction<Id>>,
    pub wire_bytes: Vec<u8>,
}

#[derive(Resource, Debug)]
pub struct GossipOutbox<Id: NetworkId = OpaqueId> {
    envelopes: Vec<TransactionEnvelope<Id>>,
}

impl<Id: NetworkId> Default for GossipOutbox<Id> {
    fn default() -> Self {
        Self {
            envelopes: Vec::new(),
        }
    }
}

impl<Id: NetworkId> GossipOutbox<Id> {
    pub fn envelopes(&self) -> &[TransactionEnvelope<Id>] {
        &self.envelopes
    }

    pub fn drain(&mut self) -> Vec<TransactionEnvelope<Id>> {
        std::mem::take(&mut self.envelopes)
    }
}

#[derive(Debug)]
pub struct ReplicationFatal<Id: NetworkId = OpaqueId> {
    pub error: ReplicationError<Id>,
}

#[derive(Resource, Debug)]
pub struct ReplicationFatalQueue<Id: NetworkId = OpaqueId> {
    errors: Vec<ReplicationFatal<Id>>,
}

impl<Id: NetworkId> Default for ReplicationFatalQueue<Id> {
    fn default() -> Self {
        Self { errors: Vec::new() }
    }
}

impl<Id: NetworkId> ReplicationFatalQueue<Id> {
    pub fn errors(&self) -> &[ReplicationFatal<Id>] {
        &self.errors
    }

    pub fn drain(&mut self) -> Vec<ReplicationFatal<Id>> {
        std::mem::take(&mut self.errors)
    }

    pub(crate) fn push(&mut self, fatal: ReplicationFatal<Id>) {
        self.errors.push(fatal);
    }
}

#[derive(Resource, Debug)]
pub struct AppliedTransactions<Id: NetworkId = OpaqueId> {
    transactions: Vec<Arc<Transaction<Id>>>,
}

impl<Id: NetworkId> Default for AppliedTransactions<Id> {
    fn default() -> Self {
        Self {
            transactions: Vec::new(),
        }
    }
}

impl<Id: NetworkId> AppliedTransactions<Id> {
    pub fn transactions(&self) -> &[Arc<Transaction<Id>>] {
        &self.transactions
    }

    pub fn clear(&mut self) {
        self.transactions.clear();
    }

    pub(crate) fn record(&mut self, transaction: Arc<Transaction<Id>>) {
        self.transactions.push(transaction);
    }
}

type ApplyFn<Id> = fn(&mut World, Entity, &Mutation<Id>) -> ReplicationResult<(), Id>;
type ResourceApplyFn<Id> = fn(&mut World, &Mutation<Id>) -> ReplicationResult<(), Id>;

#[derive(Debug, Clone)]
struct RegisteredComponent {
    type_name: &'static str,
    engine_id: EngineComponentId,
    wire_id: ComponentId,
}

#[derive(Resource)]
pub struct ComponentRegistry<Id: NetworkId = OpaqueId> {
    apply: HashMap<ComponentId, ApplyFn<Id>>,
    /// Maps a component's engine [`EngineComponentId`] to its stable wire
    /// [`ComponentId`], so mediated-capture deposits (which carry the engine id)
    /// can be recorded as wire mutations.
    wire_by_engine: HashMap<EngineComponentId, ComponentId>,
    registered: Vec<RegisteredComponent>,
}

impl<Id: NetworkId> Default for ComponentRegistry<Id> {
    fn default() -> Self {
        Self {
            apply: HashMap::new(),
            wire_by_engine: HashMap::new(),
            registered: Vec::new(),
        }
    }
}

impl<Id: NetworkId> ComponentRegistry<Id> {
    pub fn register<T: ReplicatedComponent>(&mut self) {
        self.apply.insert(T::COMPONENT_ID, apply_component::<T, Id>);
    }

    pub(crate) fn register_replicated<T: ReplicatedComponent>(
        &mut self,
        engine_id: EngineComponentId,
    ) {
        self.register::<T>();
        if !self.registered.iter().any(|component| {
            component.engine_id == engine_id && component.wire_id == T::COMPONENT_ID
        }) {
            self.registered.push(RegisteredComponent {
                type_name: std::any::type_name::<T>(),
                engine_id,
                wire_id: T::COMPONENT_ID,
            });
        }
    }

    pub(crate) fn map_engine(&mut self, engine_id: EngineComponentId, wire_id: ComponentId) {
        self.wire_by_engine.insert(engine_id, wire_id);
    }

    pub(crate) fn wire_id(&self, engine_id: EngineComponentId) -> Option<ComponentId> {
        self.wire_by_engine.get(&engine_id).copied()
    }

    #[cfg(test)]
    pub(crate) fn register_for_assertion_test<T: ReplicatedComponent>(
        &mut self,
        engine_id: EngineComponentId,
    ) {
        self.registered.push(RegisteredComponent {
            type_name: std::any::type_name::<T>(),
            engine_id,
            wire_id: T::COMPONENT_ID,
        });
    }

    pub(crate) fn apply(
        &self,
        world: &mut World,
        entity: Entity,
        mutation: &Mutation<Id>,
    ) -> ReplicationResult<(), Id> {
        let component_id = mutation.component_id();
        let apply = self
            .apply
            .get(&component_id)
            .ok_or(ReplicationError::UnknownComponent(component_id))?;
        apply(world, entity, mutation)
    }
}

#[derive(Resource)]
pub struct ResourceRegistry<Id: NetworkId = OpaqueId> {
    apply: HashMap<ResourceId, ResourceApplyFn<Id>>,
    registered: Vec<RegisteredResource>,
}

impl<Id: NetworkId> Default for ResourceRegistry<Id> {
    fn default() -> Self {
        Self {
            apply: HashMap::new(),
            registered: Vec::new(),
        }
    }
}

/// Public metadata for one registered replicated singleton resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisteredResource {
    pub type_name: &'static str,
    pub resource_id: ResourceId,
}

impl<Id: NetworkId> ResourceRegistry<Id> {
    pub fn register<T: ReplicatedResource>(&mut self) {
        let type_name = std::any::type_name::<T>();
        if self.registered.iter().any(|resource| {
            resource.type_name == type_name && resource.resource_id == T::RESOURCE_ID
        }) {
            return;
        }
        if let Some(existing) = self
            .registered
            .iter()
            .find(|resource| resource.resource_id == T::RESOURCE_ID)
        {
            panic!(
                "bevy_replicated duplicate ResourceId {:?}: {} and {} cannot both be registered",
                T::RESOURCE_ID,
                existing.type_name,
                type_name
            );
        }
        self.apply.insert(T::RESOURCE_ID, apply_resource::<T, Id>);
        self.registered.push(RegisteredResource {
            type_name,
            resource_id: T::RESOURCE_ID,
        });
    }

    /// Registered replicated resources, in registration order.
    pub fn registered(&self) -> &[RegisteredResource] {
        &self.registered
    }

    pub(crate) fn apply(
        &self,
        world: &mut World,
        mutation: &Mutation<Id>,
    ) -> ReplicationResult<(), Id> {
        let resource_id = mutation
            .resource_id()
            .expect("ResourceRegistry received component mutation");
        let apply = self
            .apply
            .get(&resource_id)
            .ok_or(ReplicationError::UnknownResource(resource_id))?;
        apply(world, mutation)
    }
}

#[derive(Resource, Debug, Default)]
struct ApplyingRemote(u32);

impl ApplyingRemote {
    fn is_active(&self) -> bool {
        self.0 > 0
    }
}

struct ApplyingRemoteGuard<'w> {
    world: &'w mut World,
    /// Mediated records captured before the guard; restored on drop. Records
    /// deposited while the guard is live (by observers the remote apply
    /// triggers) are discarded so they are not re-recorded as local deltas.
    pending: Vec<bevy::ecs::component::MediatedRecord>,
}

impl<'w> ApplyingRemoteGuard<'w> {
    fn new(world: &'w mut World) -> Self {
        world.resource_mut::<ApplyingRemote>().0 += 1;
        let pending = world
            .get_resource_mut::<MediatedCapture>()
            .map(|mut capture| capture.drain_captured().collect())
            .unwrap_or_default();
        Self { world, pending }
    }
}

impl Drop for ApplyingRemoteGuard<'_> {
    fn drop(&mut self) {
        self.world.resource_mut::<ApplyingRemote>().0 -= 1;
        if let Some(mut capture) = self.world.get_resource_mut::<MediatedCapture>() {
            capture.drain_captured().for_each(drop);
            capture.extend_captured(std::mem::take(&mut self.pending));
        }
    }
}

/// Apply state received from a peer without recording the writes as new local deltas.
pub fn without_replication_capture<R>(world: &mut World, apply: impl FnOnce(&mut World) -> R) -> R {
    if !world.contains_resource::<ApplyingRemote>() {
        return apply(world);
    }
    apply(ApplyingRemoteGuard::new(world).world)
}

fn apply_component<T: ReplicatedComponent, Id: NetworkId>(
    world: &mut World,
    entity: Entity,
    mutation: &Mutation<Id>,
) -> ReplicationResult<(), Id> {
    if let Some(payload) = mutation.payload_bytes() {
        let value = T::decode_payload(payload)?;
        let value = T::merge_remote(world.get::<T>(entity), value);
        ApplyingRemoteGuard::new(world)
            .world
            .entity_mut(entity)
            .insert(value);
    } else {
        ApplyingRemoteGuard::new(world)
            .world
            .entity_mut(entity)
            .remove::<T>();
    }
    Ok(())
}

fn apply_resource<T: ReplicatedResource, Id: NetworkId>(
    world: &mut World,
    mutation: &Mutation<Id>,
) -> ReplicationResult<(), Id> {
    if let Some(payload) = mutation.payload_bytes() {
        let value = T::decode_payload(payload)?;
        let value = T::merge_remote(world.get_resource::<T>(), value);
        ApplyingRemoteGuard::new(world).world.insert_resource(value);
    } else {
        ApplyingRemoteGuard::new(world).world.remove_resource::<T>();
    }
    Ok(())
}

/// Captures structural inserts of replicated components as the sole structural
/// recorder: any direct `entity_mut().insert::<T>()` on a bound entity is recorded.
///
/// Three inserts are deliberately NOT recorded:
/// - **Remote/self applies** (`apply_component`) run under `ApplyingRemote`; the
///   world change there is the *result* of a transaction, not a new delta.
/// - **Unbound entities** have no network identity yet (pre-binding spawn order).
/// - **Births**: a component added at or before the entity's baseline tick is part
///   of the deterministic spawn (or same-tick pre-state). Both peers reconstruct it
///   by spawning identically, so recording it would be a spurious, divergent delta.
///
/// Recorded inserts go to the frame's structural batch so a frame's structural
/// changes form one atomic transaction.
fn insert_hook<T: ReplicatedComponent, Id: NetworkId>(
    mut world: DeferredWorld,
    HookContext { entity, .. }: HookContext,
) {
    if world
        .get_resource::<ApplyingRemote>()
        .is_some_and(|guard| guard.is_active())
    {
        return;
    }

    let current = world.change_tick();
    let Some((network_id, baseline)) =
        world
            .get_resource::<EntityReplicas<Id>>()
            .and_then(|entities| {
                entities
                    .network_id(entity)
                    .map(|network_id| (network_id, entities.baseline_tick(entity)))
            })
    else {
        return;
    };

    // Suppress the entity's birth: a component added at or before its baseline tick
    // is part of the deterministic spawn, not a delta.
    let is_baseline = baseline.is_some_and(|baseline| {
        world
            .entity_mut(entity)
            .get_change_ticks::<T>()
            .is_some_and(|ticks| !ticks.added.is_newer_than(baseline, current))
    });
    if is_baseline {
        return;
    }

    let Some(payload) = world
        .get::<T>(entity)
        .map(|component| component.encode_payload())
    else {
        return;
    };
    let payload = match payload {
        Ok(payload) => payload,
        Err(error) => {
            log::error!(
                "failed to encode replicated insert for component {}: {error}",
                std::any::type_name::<T>()
            );
            return;
        }
    };

    let Some(mut buffer) = world.get_resource_mut::<TransactionBuffer<Id>>() else {
        return;
    };
    buffer.record_structural(Mutation::insert(network_id, T::COMPONENT_ID, payload));
}

/// Captures structural removes of replicated components from bound entities.
///
/// Removes are always post-baseline deltas (a birth bundle never removes), so unlike
/// [`insert_hook`] there is no baseline check; only `ApplyingRemote` (self/remote
/// apply) and unbound entities are skipped.
fn remove_hook<T: ReplicatedComponent, Id: NetworkId>(
    mut world: DeferredWorld,
    HookContext { entity, .. }: HookContext,
) {
    if world
        .get_resource::<ApplyingRemote>()
        .is_some_and(|guard| guard.is_active())
    {
        return;
    }

    let Some(network_id) = world
        .get_resource::<EntityReplicas<Id>>()
        .and_then(|entities| entities.network_id(entity))
    else {
        return;
    };

    let Some(mut buffer) = world.get_resource_mut::<TransactionBuffer<Id>>() else {
        return;
    };
    buffer.record_structural(Mutation::remove(network_id, T::COMPONENT_ID));
}

/// Type-erased post-image encoder registered with the engine's [`MediatedCapture`]
/// for component `T`. Invoked on every closure-only in-place mutation of `T`.
///
/// # Safety
///
/// `ptr` must reference a live value of type `T`. The engine upholds this by only
/// invoking the encoder keyed by `T`'s engine component id with a pointer to `T`.
#[allow(
    unsafe_code,
    reason = "MediatedCapture provides a type-erased component pointer"
)]
unsafe fn encode_mediated<T: ReplicatedComponent>(ptr: bevy::ecs::ptr::Ptr) -> Vec<u8> {
    // SAFETY: guaranteed by the function's contract (see above).
    let value = unsafe { ptr.deref::<T>() };
    value.encode_payload().unwrap_or_default()
}

/// Drains the engine's mediated-capture queue into the transaction buffer.
///
/// This is the completeness safety-net for the live-mutation path: any consumer
/// who mutates a replicated component through `MediatedMut::modify` is recorded,
/// because the engine deposited the post-image.
fn drain_mediated_capture<Id: NetworkId>(
    mut capture: ResMut<MediatedCapture>,
    registry: Res<ComponentRegistry<Id>>,
    entities: Res<EntityReplicas<Id>>,
    filter: Res<LiveReplicationFilter>,
    mut buffer: ResMut<TransactionBuffer<Id>>,
) {
    if !capture.has_captured() {
        return;
    }
    let records: Vec<_> = capture.drain_captured().collect();
    // All live mediated writes captured in this PostUpdate drain share one frame
    // transaction.
    buffer.begin_batch();
    for record in records {
        let Some(network_id) = entities.network_id(record.entity) else {
            continue;
        };
        let Some(wire_id) = registry.wire_id(record.component_id) else {
            continue;
        };
        if filter.suppresses_modify_component(wire_id) {
            continue;
        }
        buffer.record(Mutation::modify(network_id, wire_id, record.post_image));
    }
    buffer.finish_batch();
}

/// Commits the frame's structural inserts/removes (captured by the lifecycle hooks)
/// as a single atomic transaction. Runs after `drain_mediated_capture` and before
/// the flush so the frame's structural changes are one chain entry.
fn commit_structural_frame<Id: NetworkId>(mut buffer: ResMut<TransactionBuffer<Id>>) {
    buffer.commit_structural_frame();
}

fn assert_registered_components_complete<Id: NetworkId>(
    registry: Res<ComponentRegistry<Id>>,
    capture: Res<MediatedCapture>,
) {
    for component in &registry.registered {
        if !registry.apply.contains_key(&component.wire_id) {
            panic!(
                "bevy_replicated registration incomplete for {} (wire ComponentId {:?}): missing apply registration",
                component.type_name, component.wire_id
            );
        }
        match registry.wire_by_engine.get(&component.engine_id) {
            Some(wire_id) if *wire_id == component.wire_id => {}
            Some(wire_id) => {
                panic!(
                    "bevy_replicated registration incomplete for {} (wire ComponentId {:?}, engine ComponentId {:?}): engine mapping points to {:?}",
                    component.type_name, component.wire_id, component.engine_id, wire_id
                );
            }
            None => {
                panic!(
                    "bevy_replicated registration incomplete for {} (wire ComponentId {:?}, engine ComponentId {:?}): missing engine-to-wire mapping",
                    component.type_name, component.wire_id, component.engine_id
                );
            }
        }
        if capture.encoder(component.engine_id).is_none() {
            panic!(
                "bevy_replicated registration incomplete for {} (wire ComponentId {:?}, engine ComponentId {:?}): missing mediated encoder",
                component.type_name, component.wire_id, component.engine_id
            );
        }
    }
}

#[derive(SystemParam)]
pub struct ReplicatedResMut<'w, T: ReplicatedResource, Id: NetworkId = OpaqueId> {
    value: ResMut<'w, T>,
    buffer: ResMut<'w, TransactionBuffer<Id>>,
}

impl<T: ReplicatedResource, Id: NetworkId> ReplicatedResMut<'_, T, Id> {
    /// Read the current resource value.
    pub fn get(&self) -> &T {
        &self.value
    }

    /// Mutate the resource and record the resulting post-image.
    pub fn modify(&mut self, f: impl FnOnce(&mut T)) -> ReplicationResult<(), Id> {
        f(&mut self.value);
        let payload = self.value.encode_payload()?;
        self.buffer
            .record(Mutation::modify_resource(T::RESOURCE_ID, payload));
        Ok(())
    }

    /// Replace the resource and record the new post-image as a modify mutation.
    pub fn replace(&mut self, value: T) -> ReplicationResult<T, Id> {
        let payload = value.encode_payload()?;
        let previous = std::mem::replace(&mut *self.value, value);
        self.buffer
            .record(Mutation::modify_resource(T::RESOURCE_ID, payload));
        Ok(previous)
    }

    /// Record the current resource value as a modify mutation.
    ///
    /// Use this after an intentional in-place change performed through another
    /// scoped API. Ordinary `ResMut<T>` writes are not automatically captured.
    pub fn record_current(&mut self) -> ReplicationResult<(), Id> {
        let payload = self.value.encode_payload()?;
        self.buffer
            .record(Mutation::modify_resource(T::RESOURCE_ID, payload));
        Ok(())
    }
}

pub trait ReplicatedAppExt<Id: NetworkId = OpaqueId> {
    fn register_replicated_component<T: ReplicatedComponent + MediatedAccess>(
        &mut self,
    ) -> &mut Self;
    fn register_replicated_resource<T: ReplicatedResource>(&mut self) -> &mut Self;
    fn exchange_chain_hash_on_state_transition<S: States>(&mut self) -> &mut Self;
}

impl<Id: NetworkId> ReplicatedAppExt<Id> for App {
    fn register_replicated_component<T: ReplicatedComponent + MediatedAccess>(
        &mut self,
    ) -> &mut Self {
        if !self.world().contains_resource::<ComponentRegistry<Id>>() {
            self.init_resource::<ComponentRegistry<Id>>();
        }
        if !self.world().contains_resource::<MediatedCapture>() {
            self.init_resource::<MediatedCapture>();
        }
        // Engine id ↔ wire id, and the post-image encoder for the engine's
        // closure-only `MediatedMut` capture path.
        let engine_id = self.world_mut().register_component::<T>();
        self.world_mut()
            .register_component_hooks::<T>()
            .on_insert(insert_hook::<T, Id>)
            .on_remove(remove_hook::<T, Id>);
        self.world_mut()
            .resource_mut::<MediatedCapture>()
            .register_encoder(engine_id, encode_mediated::<T>);
        let mut registry = self.world_mut().resource_mut::<ComponentRegistry<Id>>();
        registry.register_replicated::<T>(engine_id);
        registry.map_engine(engine_id, T::COMPONENT_ID);
        self
    }

    fn register_replicated_resource<T: ReplicatedResource>(&mut self) -> &mut Self {
        if !self.world().contains_resource::<ResourceRegistry<Id>>() {
            self.init_resource::<ResourceRegistry<Id>>();
        }
        self.world_mut()
            .resource_mut::<ResourceRegistry<Id>>()
            .register::<T>();
        self
    }

    fn exchange_chain_hash_on_state_transition<S: States>(&mut self) -> &mut Self {
        self.add_systems(
            StateTransition,
            report_chain_hash_on_state_transition::<S>
                .after(StateTransitionSystems::DependentTransitions)
                .before(StateTransitionSystems::ExitSchedules),
        )
    }
}

pub struct ReplicatedPlugin<
    Id: NetworkId = OpaqueId,
    O: OrderingPolicy = ReliableOrdered,
    C: ConsistencyModel = CanonicalEventual,
> {
    chain_history_capacity: ChainHistoryCapacity,
    marker: PhantomData<(Id, O, C)>,
}

impl<Id: NetworkId, O: OrderingPolicy, C: ConsistencyModel> Default for ReplicatedPlugin<Id, O, C> {
    fn default() -> Self {
        Self {
            chain_history_capacity: ChainHistoryCapacity::default(),
            marker: PhantomData,
        }
    }
}

impl<Id: NetworkId, O: OrderingPolicy, C: ConsistencyModel> ReplicatedPlugin<Id, O, C> {
    pub fn with_chain_history_capacity(capacity: ChainHistoryCapacity) -> Self {
        Self {
            chain_history_capacity: capacity,
            marker: PhantomData,
        }
    }
}

impl<Id: TransactionWireId, O: OrderingPolicy, C: ConsistencyModel> Plugin
    for ReplicatedPlugin<Id, O, C>
{
    fn build(&self, app: &mut App) {
        app.insert_resource(self.chain_history_capacity)
            .insert_resource(ReplicationChain::with_history_capacity(
                self.chain_history_capacity,
            ))
            .init_resource::<ReplicationIdentity>()
            .init_resource::<ReplicationEngine<O, C>>()
            .init_resource::<EntityReplicas<Id>>()
            .init_resource::<TransactionBuffer<Id>>()
            .init_resource::<ReplicationParticipants>()
            .init_resource::<GapDetectionConfig>()
            .init_resource::<TransactionGapAlerts>()
            .init_resource::<GossipOutbox<Id>>()
            .init_resource::<ReplicationFatalQueue<Id>>()
            .init_resource::<AppliedTransactions<Id>>()
            .init_resource::<LiveReplicationFilter>()
            .init_resource::<ChainHashConfig>()
            .init_resource::<ChainHashOutbox>()
            .init_resource::<ChainHashExchangeState>()
            .init_resource::<RemoteChainHashReports>()
            .init_resource::<ComponentRegistry<Id>>()
            .init_resource::<ResourceRegistry<Id>>()
            .init_resource::<MediatedCapture>()
            .init_resource::<ApplyingRemote>()
            .add_message::<DivergenceEvent>()
            .add_message::<ChainHashCheckpoint>()
            .add_systems(Startup, assert_registered_components_complete::<Id>)
            .add_systems(PreUpdate, process_chain_hash_checkpoints);

        if TypeId::of::<C>() == TypeId::of::<AuthorityOrdered>() {
            app.init_resource::<AuthorityPeer>()
                .init_resource::<AuthorityOrderedRole>()
                .init_resource::<AuthorityOrderedProposals<Id>>();
        }

        if C::OWNS_SCHEDULE {
            <C::Scheduler as ScheduleDriver>::build(app);
        } else {
            app.add_systems(PreUpdate, apply_ready_transactions::<Id, C>)
                .add_systems(
                    PostUpdate,
                    (
                        sync_peer_id::<Id>,
                        SyncBarrier::<ReplicatedSet>::system,
                        drain_mediated_capture::<Id>,
                        commit_structural_frame::<Id>,
                        flush_transactions::<Id>,
                    )
                        .chain(),
                );
        }
    }
}

fn report_chain_hash_on_state_transition<S: States>(
    mut transitions: MessageReader<StateTransitionEvent<S>>,
    config: Res<ChainHashConfig>,
    mut state: ResMut<ChainHashExchangeState>,
    identity: Res<ReplicationIdentity>,
    chain: Res<ReplicationChain>,
    mut outbox: ResMut<ChainHashOutbox>,
) {
    if config.report_on_phase_boundary && transitions.read().next().is_some() {
        push_report(
            &mut state,
            &mut outbox,
            ChainHashReport::new(identity.peer_id, chain.applied_count(), chain.current()),
        )
        .expect("chain hash report serialization failed");
    }
}

fn sync_peer_id<Id: NetworkId>(
    identity: Res<ReplicationIdentity>,
    mut buffer: ResMut<TransactionBuffer<Id>>,
) {
    buffer.set_peer_id(identity.peer_id);
}

fn flush_transactions<Id: TransactionWireId>(world: &mut World) {
    if let Err(error) = flush_transactions_inner::<Id>(world) {
        record_replication_fatal(world, "bevy_replicated transaction flush failed", error);
    }
}

fn flush_transactions_inner<Id: TransactionWireId>(world: &mut World) -> ReplicationResult<(), Id> {
    let transactions = world
        .resource_mut::<TransactionBuffer<Id>>()
        .drain_committed_transactions();
    for transaction in transactions {
        let transaction = Arc::new(transaction);
        let wire_bytes = encode_transaction(transaction.as_ref())?;
        world
            .resource_mut::<TransactionBuffer<Id>>()
            .enqueue_local_transaction(Arc::clone(&transaction), wire_bytes.clone())?;
        world
            .resource_mut::<ReplicationParticipants>()
            .observe(transaction.peer_id);
        world
            .resource_mut::<GossipOutbox<Id>>()
            .envelopes
            .push(TransactionEnvelope {
                transaction,
                wire_bytes,
            });
    }
    Ok(())
}

pub fn commit_now(world: &mut World) -> ReplicationResult<()> {
    commit_now_as::<OpaqueId>(world)
}

pub fn commit_now_as<Id: TransactionWireId>(world: &mut World) -> ReplicationResult<(), Id> {
    SyncBarrier::<ReplicatedSet>::system(world);
    flush_transactions_inner::<Id>(world)?;
    apply_ready_transactions_inner::<Id, CanonicalEventual>(world)
}

pub fn insert_replicated_resource<T: ReplicatedResource>(
    world: &mut World,
    value: T,
) -> ReplicationResult<()> {
    insert_replicated_resource_as::<OpaqueId, T>(world, value)
}

/// Insert a replicated resource and emit an `InsertResource` mutation.
pub fn insert_replicated_resource_as<Id: NetworkId, T: ReplicatedResource>(
    world: &mut World,
    value: T,
) -> ReplicationResult<(), Id> {
    let payload = value.encode_payload()?;
    world.insert_resource(value);
    world
        .resource_mut::<TransactionBuffer<Id>>()
        .record(Mutation::insert_resource(T::RESOURCE_ID, payload));
    Ok(())
}

pub fn remove_replicated_resource<T: ReplicatedResource>(
    world: &mut World,
) -> ReplicationResult<Option<T>> {
    remove_replicated_resource_as::<OpaqueId, T>(world)
}

/// Remove a replicated resource and emit `RemoveResource` only when present.
pub fn remove_replicated_resource_as<Id: NetworkId, T: ReplicatedResource>(
    world: &mut World,
) -> ReplicationResult<Option<T>, Id> {
    let removed = world.remove_resource::<T>();
    if removed.is_some() {
        world
            .resource_mut::<TransactionBuffer<Id>>()
            .record(Mutation::remove_resource(T::RESOURCE_ID));
    }
    Ok(removed)
}

pub(crate) fn record_replication_fatal<Id: NetworkId>(
    world: &mut World,
    context: &'static str,
    error: ReplicationError<Id>,
) {
    log::error!(
        target: "bevy_replicated::fatal",
        "{}; queuing fatal replication error: {}",
        context,
        error
    );
    world
        .resource_mut::<ReplicationFatalQueue<Id>>()
        .push(ReplicationFatal { error });
}

pub fn receive_transaction<Id: NetworkId>(
    world: &mut World,
    transaction: Transaction<Id>,
) -> ReplicationResult<ReceiveTransactionStatus, Id> {
    let peer_id = transaction.peer_id;
    let status = world
        .resource_mut::<TransactionBuffer<Id>>()
        .receive_transaction(transaction)?;
    world
        .resource_mut::<ReplicationParticipants>()
        .observe(peer_id);
    Ok(status)
}
