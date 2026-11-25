// intentionally oversize: release-gate baseline; split requires scoped semantic extraction.
use std::error::Error as _;

use bevy::ecs::entity::{EntityGeneration, InvalidEntityError};
use bevy::ecs::query::QueryEntityError;
use bevy::ecs::schedule::{IntoScheduleConfigs, common_conditions::run_once};
use bevy::prelude::*;

use super::*;

#[derive(
    bevy::ecs::component::MediatedAccess,
    Debug,
    Clone,
    PartialEq,
    Eq,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
struct TestHealth(u32);

impl ReplicatedComponent for TestHealth {
    const COMPONENT_ID: ComponentId = ComponentId(1);

    fn encode_payload(&self) -> Result<Vec<u8>, WireError> {
        rkyv::to_bytes::<rkyv::rancor::Error>(self)
            .map(|bytes| bytes.to_vec())
            .map_err(WireError::Encode)
    }

    fn decode_payload(bytes: &[u8]) -> Result<Self, WireError> {
        rkyv::from_bytes::<Self, rkyv::rancor::Error>(bytes).map_err(WireError::Decode)
    }
}

#[derive(Resource)]
struct Target(Entity);

#[derive(Resource, Default)]
struct MediatedAccessObserved(bool);

fn mutate_health(mut health: bevy::ecs::system::MediatedMut<TestHealth>, target: Res<Target>) {
    health
        .modify(target.0, |health| health.0 = 7)
        .expect("TestHealth mediated modify failed");
}

fn mutate_health_batch(
    mut health: bevy::ecs::system::MediatedMut<TestHealth>,
    target: Res<Target>,
) {
    health
        .modify(target.0, |health| health.0 = 8)
        .expect("TestHealth mediated modify failed");
    health
        .modify(target.0, |health| health.0 = 9)
        .expect("TestHealth mediated modify failed");
}

fn observe_mediated_access_error(
    mut health: bevy::ecs::system::MediatedMut<TestHealth>,
    target: Res<Target>,
    mut observed: ResMut<MediatedAccessObserved>,
) {
    let error = health
        .modify(target.0, |_| ())
        .expect_err("entity without TestHealth should fail mediated access");
    observed.0 = matches!(
        error,
        QueryEntityError::QueryDoesNotMatch(entity, _) if entity == target.0
    );
}

#[test]
fn transaction_round_trips_through_rkyv() {
    let transaction = Transaction::new(
        ReplicationPeerId([5; 32]),
        42,
        vec![Mutation::modify(OpaqueId(1), ComponentId(7), vec![1, 2, 3])],
    );

    let bytes = encode_transaction(&transaction).expect("transaction encodes");
    let decoded = decode_transaction(&bytes).expect("transaction decodes");

    assert_eq!(decoded, transaction);
}

#[test]
fn policy_marker_does_not_change_transaction_bytes_or_chain_hash() {
    let transaction = Transaction::new(
        ReplicationPeerId([5; 32]),
        42,
        vec![Mutation::modify(OpaqueId(1), ComponentId(7), vec![1, 2, 3])],
    );

    let bytes = encode_transaction(&transaction).expect("transaction encodes");
    let mut chain = ReplicationChain::new();
    chain.fold(&transaction).expect("transaction folds");

    assert_eq!(
        bytes,
        vec![
            1, 2, 3, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0,
            228, 255, 255, 255, 3, 0, 0, 0, 0, 0, 0, 0, 3, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
            5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 0, 0, 0, 0, 0, 0, 0, 42, 0, 0,
            0, 0, 0, 0, 0, 176, 255, 255, 255, 1, 0, 0, 0
        ]
    );
    assert_eq!(
        chain.current(),
        [
            193, 89, 138, 78, 60, 144, 239, 254, 184, 16, 4, 216, 75, 195, 106, 59, 0, 218, 157,
            127, 123, 0, 39, 62, 236, 213, 246, 252, 120, 232, 25, 90
        ]
    );
}

#[test]
fn mediated_mut_records_to_transaction() {
    let mut app = test_app();
    app.add_systems(Update, mutate_health.run_if(run_once));

    let entity = spawn_replicated_health(&mut app, 3);
    app.world_mut().insert_resource(Target(entity));

    let genesis = app.world().resource::<ReplicationChain>().current();
    app.update();

    let health = app
        .world()
        .get::<TestHealth>(entity)
        .expect("health exists");
    assert_eq!(health.0, 7);
    let outbox = app.world().resource::<GossipOutbox>();
    assert_eq!(outbox.envelopes().len(), 1);
    assert_eq!(outbox.envelopes()[0].transaction.mutations.len(), 1);
    assert_eq!(
        app.world()
            .resource::<AppliedTransactions>()
            .transactions()
            .len(),
        0
    );
    assert_eq!(
        app.world().resource::<ReplicationChain>().current(),
        genesis
    );

    app.update();

    let health = app
        .world()
        .get::<TestHealth>(entity)
        .expect("health exists");
    assert_eq!(health.0, 7);
    assert_ne!(
        app.world().resource::<ReplicationChain>().current(),
        genesis
    );
    assert_eq!(app.world().resource::<ReplicationChain>().count(), 1);
}

#[test]
fn live_replication_filter_suppresses_mediated_modify_capture() {
    let mut app = test_app();
    app.add_systems(Update, mutate_health.run_if(run_once));
    app.world_mut()
        .resource_mut::<LiveReplicationFilter>()
        .suppress_modify_component(TestHealth::COMPONENT_ID);

    let entity = spawn_replicated_health(&mut app, 3);
    app.world_mut().insert_resource(Target(entity));

    app.update();

    let health = app
        .world()
        .get::<TestHealth>(entity)
        .expect("health exists");
    assert_eq!(health.0, 7);
    assert!(
        app.world()
            .resource::<GossipOutbox>()
            .envelopes()
            .is_empty()
    );
    assert_eq!(app.world().resource::<ReplicationChain>().count(), 0);
}

#[test]
fn observer_writes_under_remote_apply_are_not_recaptured() {
    #[derive(Event)]
    struct Bump(Entity, u32);

    let mut app = test_app();
    let entity = spawn_replicated_health(&mut app, 3);
    app.add_observer(
        |bump: On<Bump>, mut health: bevy::ecs::system::MediatedMut<TestHealth>| {
            health.modify(bump.0, |health| health.0 = bump.1).unwrap();
        },
    );

    without_replication_capture(app.world_mut(), |world| world.trigger(Bump(entity, 5)));
    app.update();
    assert_eq!(app.world().get::<TestHealth>(entity), Some(&TestHealth(5)));
    assert!(
        app.world()
            .resource::<GossipOutbox>()
            .envelopes()
            .is_empty()
    );

    // Outside remote apply, the same observer write is captured.
    app.world_mut().trigger(Bump(entity, 6));
    app.update();
    assert_eq!(app.world().resource::<GossipOutbox>().envelopes().len(), 1);
}

#[test]
fn direct_remove_on_bound_entity_records_remove_transaction() {
    let mut app = test_app();
    let entity = spawn_replicated_health(&mut app, 3);

    app.world_mut().entity_mut(entity).remove::<TestHealth>();
    app.update();

    assert!(app.world().get::<TestHealth>(entity).is_none());
    let outbox = app.world().resource::<GossipOutbox>();
    assert_eq!(outbox.envelopes().len(), 1);
    assert_eq!(
        outbox.envelopes()[0].transaction.mutations,
        vec![Mutation::remove(OpaqueId(0), TestHealth::COMPONENT_ID)]
    );
}

#[test]
fn direct_remove_on_unbound_entity_records_nothing() {
    let mut app = test_app();
    let entity = app.world_mut().spawn(TestHealth(3)).id();

    app.world_mut().entity_mut(entity).remove::<TestHealth>();
    app.update();

    assert!(app.world().get::<TestHealth>(entity).is_none());
    assert!(
        app.world()
            .resource::<GossipOutbox>()
            .envelopes()
            .is_empty()
    );
}

#[test]
fn direct_insert_on_bound_fresh_entity_records_insert_transaction() {
    let mut app = test_app();
    let entity = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<EntityReplicas>()
        .bind(entity, OpaqueId(0));

    app.world_mut().entity_mut(entity).insert(TestHealth(5));
    app.update();

    assert_eq!(app.world().get::<TestHealth>(entity).unwrap().0, 5);
    let outbox = app.world().resource::<GossipOutbox>();
    assert_eq!(outbox.envelopes().len(), 1);
    assert_eq!(
        outbox.envelopes()[0].transaction.mutations,
        vec![Mutation::insert(
            OpaqueId(0),
            TestHealth::COMPONENT_ID,
            health_payload(5)
        )]
    );
}

#[test]
fn local_insert_self_echo_does_not_overwrite_newer_local_state() {
    let mut app = test_app();
    let entity = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<EntityReplicas>()
        .bind(entity, OpaqueId(0));

    app.world_mut().entity_mut(entity).insert(TestHealth(5));
    app.update();

    assert_eq!(app.world().get::<TestHealth>(entity).unwrap().0, 5);
    assert_eq!(app.world().resource::<GossipOutbox>().envelopes().len(), 1);
    assert_eq!(
        app.world()
            .resource::<AppliedTransactions>()
            .transactions()
            .len(),
        0
    );

    app.world_mut().entity_mut(entity).insert(TestHealth(9));
    app.update();

    assert_eq!(
        app.world().get::<TestHealth>(entity).unwrap().0,
        9,
        "local sequence 0 carried TestHealth(5), but it must not replay over a newer local value"
    );
    assert_eq!(app.world().resource::<GossipOutbox>().envelopes().len(), 2);
    assert_eq!(
        app.world()
            .resource::<AppliedTransactions>()
            .transactions()
            .len(),
        1
    );

    app.update();

    assert_eq!(app.world().get::<TestHealth>(entity).unwrap().0, 9);
    assert_eq!(
        app.world()
            .resource::<AppliedTransactions>()
            .transactions()
            .len(),
        2
    );
}

#[test]
fn direct_overwrite_on_bound_entity_records_new_value_once() {
    let mut app = test_app();
    let entity = spawn_replicated_health(&mut app, 3);

    app.world_mut().entity_mut(entity).insert(TestHealth(9));
    app.update();

    assert_eq!(app.world().get::<TestHealth>(entity).unwrap().0, 9);
    let outbox = app.world().resource::<GossipOutbox>();
    assert_eq!(outbox.envelopes().len(), 1);
    assert_eq!(
        outbox.envelopes()[0].transaction.mutations,
        vec![Mutation::insert(
            OpaqueId(0),
            TestHealth::COMPONENT_ID,
            health_payload(9)
        )]
    );
}

#[test]
fn direct_insert_on_unbound_entity_records_nothing_then_bound_overwrite_records() {
    let mut app = test_app();
    let entity = app.world_mut().spawn(TestHealth(3)).id();

    app.update();

    assert_eq!(app.world().get::<TestHealth>(entity).unwrap().0, 3);
    assert!(
        app.world()
            .resource::<GossipOutbox>()
            .envelopes()
            .is_empty()
    );

    app.world_mut()
        .resource_mut::<EntityReplicas>()
        .bind(entity, OpaqueId(0));
    app.world_mut().entity_mut(entity).insert(TestHealth(4));
    app.update();

    assert_eq!(app.world().get::<TestHealth>(entity).unwrap().0, 4);
    let outbox = app.world().resource::<GossipOutbox>();
    assert_eq!(outbox.envelopes().len(), 1);
    assert_eq!(
        outbox.envelopes()[0].transaction.mutations,
        vec![Mutation::insert(
            OpaqueId(0),
            TestHealth::COMPONENT_ID,
            health_payload(4)
        )]
    );
}

#[test]
fn received_remove_apply_does_not_echo_to_outbox() {
    let mut app = test_app();
    let entity = spawn_replicated_health(&mut app, 3);
    let transaction = Transaction::new(
        ReplicationPeerId([9; 32]),
        0,
        vec![Mutation::remove(OpaqueId(0), TestHealth::COMPONENT_ID)],
    );

    receive_transaction(app.world_mut(), transaction).expect("remove transaction buffers");
    app.update();

    assert!(app.world().get::<TestHealth>(entity).is_none());
    assert_eq!(
        app.world()
            .resource::<AppliedTransactions>()
            .transactions()
            .len(),
        1
    );
    assert!(
        app.world()
            .resource::<GossipOutbox>()
            .envelopes()
            .is_empty()
    );
}

#[test]
fn received_insert_apply_does_not_echo_to_outbox_and_peers_converge() {
    let mut left = test_app();
    let mut right = test_app();
    let left_entity = spawn_replicated_health(&mut left, 3);
    let right_entity = spawn_replicated_health(&mut right, 1);
    let transaction = Transaction::new(
        ReplicationPeerId([9; 32]),
        0,
        vec![Mutation::insert(
            OpaqueId(0),
            TestHealth::COMPONENT_ID,
            health_payload(7),
        )],
    );

    receive_transaction(left.world_mut(), transaction.clone()).expect("left insert buffers");
    receive_transaction(right.world_mut(), transaction).expect("right insert buffers");
    left.update();
    right.update();

    assert_eq!(left.world().get::<TestHealth>(left_entity).unwrap().0, 7);
    assert_eq!(
        left.world().get::<TestHealth>(left_entity),
        right.world().get::<TestHealth>(right_entity)
    );
    assert_eq!(
        left.world()
            .resource::<AppliedTransactions>()
            .transactions()
            .len(),
        1
    );
    assert_eq!(
        right
            .world()
            .resource::<AppliedTransactions>()
            .transactions()
            .len(),
        1
    );
    assert!(
        left.world()
            .resource::<GossipOutbox>()
            .envelopes()
            .is_empty()
    );
    assert!(
        right
            .world()
            .resource::<GossipOutbox>()
            .envelopes()
            .is_empty()
    );
}

#[test]
fn replication_error_source_chain_preserves_mediated_access_source() {
    let wire_error = decode_transaction::<OpaqueId>(&[0]).expect_err("invalid bytes fail");
    let error = ReplicationError::<OpaqueId>::Wire(wire_error);
    assert!(error.source().is_some());

    let ordering = CanonicalOrderingError::DuplicateTransactionKey(TransactionKey::new(
        ReplicationPeerId([1; 32]),
        7,
    ));
    let error = ReplicationError::<OpaqueId>::CanonicalOrdering(ordering);
    assert!(error.source().is_some());

    let query_error = QueryEntityError::NotSpawned(
        InvalidEntityError {
            entity: Entity::PLACEHOLDER,
            current_generation: EntityGeneration::FIRST,
        }
        .into(),
    );
    let error = ReplicationError::<OpaqueId>::MediatedAccess(query_error);
    let source = error.source().expect("mediated access source is kept");

    assert!(source.is::<QueryEntityError>());
    assert!(
        source.source().is_some(),
        "QueryEntityError::NotSpawned keeps its entity source"
    );
}

#[test]
fn wire_error_decode_carries_rkyv_source() {
    let error = decode_transaction::<OpaqueId>(&[0]).expect_err("invalid bytes fail");
    assert!(matches!(error, WireError::Decode(_)));
    assert!(error.source().is_some());
}

#[test]
fn wire_error_invariant_broken_is_reported_without_source() {
    let error = WireError::InvariantBroken {
        detail: "missing chain hash at count 7".to_owned(),
    };

    assert!(matches!(error, WireError::InvariantBroken { .. }));
    assert_eq!(
        error.to_string(),
        "replication invariant broken: missing chain hash at count 7"
    );
    assert!(error.source().is_none());
}

#[test]
fn batch_collects_multiple_mutations_atomically() {
    let mut app = test_app();
    app.add_systems(Update, mutate_health_batch.run_if(run_once));

    let entity = spawn_replicated_health(&mut app, 3);
    app.world_mut().insert_resource(Target(entity));

    app.update();
    app.update();

    let outbox = app.world().resource::<GossipOutbox>();
    assert_eq!(outbox.envelopes().len(), 1);
    assert_eq!(outbox.envelopes()[0].transaction.mutations.len(), 2);
    assert_eq!(
        app.world()
            .resource::<AppliedTransactions>()
            .transactions()
            .len(),
        1
    );
}

#[test]
fn flush_transactions_arc_shares_payload() {
    let mut app = test_app();
    app.add_systems(Update, mutate_health_batch.run_if(run_once));

    let entity = spawn_replicated_health(&mut app, 3);
    app.world_mut().insert_resource(Target(entity));

    app.update();
    let outbox_transaction = app.world().resource::<GossipOutbox>().envelopes()[0]
        .transaction
        .clone();

    app.update();
    let applied_transaction =
        app.world().resource::<AppliedTransactions>().transactions()[0].clone();

    assert!(std::sync::Arc::ptr_eq(
        &outbox_transaction,
        &applied_transaction
    ));
}

#[test]
fn apply_no_per_tick_clone_alloc() {
    let mut app = test_app();
    crate::transaction::reset_replication_participants_clone_count();

    for _ in 0..100 {
        app.update();
    }

    assert_eq!(
        crate::transaction::replication_participants_clone_count(),
        100
    );
}

#[test]
fn mediated_mut_reports_typed_access_error() {
    let mut app = test_app();
    app.init_resource::<MediatedAccessObserved>();
    app.add_systems(Update, observe_mediated_access_error.run_if(run_once));

    let entity = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<EntityReplicas>()
        .bind(entity, OpaqueId(0));
    app.world_mut().insert_resource(Target(entity));

    app.update();

    assert!(app.world().resource::<MediatedAccessObserved>().0);
    assert!(
        app.world()
            .resource::<GossipOutbox>()
            .envelopes()
            .is_empty()
    );
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn plugin_initializes_required_resources() {
    let app = test_app();

    assert!(app.world().contains_resource::<EntityReplicas>());
    assert!(app.world().contains_resource::<TransactionBuffer>());
    assert!(app.world().contains_resource::<ReplicationParticipants>());
    assert!(app.world().contains_resource::<GapDetectionConfig>());
    assert!(app.world().contains_resource::<TransactionGapAlerts>());
    assert!(app.world().contains_resource::<GossipOutbox>());
    assert!(app.world().contains_resource::<ReplicationFatalQueue>());
    assert!(app.world().contains_resource::<ReplicationChain>());
    assert!(app.world().contains_resource::<ChainHistoryCapacity>());
    assert!(app.world().contains_resource::<CanonicalEventualEngine>());
    assert_eq!(ReliableOrdered::CHANNEL_KIND, ChannelKind::ReliableOrdered);
    assert_eq!(CanonicalEventual::LABEL, "CanonicalEventual");
    assert!(!CanonicalEventual::OWNS_SCHEDULE);
}

#[test]
fn apply_failure_surfaces_through_fatal_queue() {
    let mut app = test_app();

    receive_transaction(
        app.world_mut(),
        Transaction::new(
            ReplicationPeerId([9; 32]),
            0,
            vec![Mutation::modify(
                OpaqueId(404),
                TestHealth::COMPONENT_ID,
                health_payload(1),
            )],
        ),
    )
    .expect("transaction buffers");

    app.update();

    let errors = app.world().resource::<ReplicationFatalQueue>().errors();
    assert_eq!(errors.len(), 1);
    assert!(matches!(
        errors[0].error,
        ReplicationError::MissingLocalEntity(OpaqueId(404))
    ));
}

#[test]
fn stale_remove_for_despawned_replica_is_skipped() {
    let mut app = test_app();
    let entity = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<EntityReplicas>()
        .bind(entity, OpaqueId(0));
    app.world_mut().entity_mut(entity).despawn();

    receive_transaction(
        app.world_mut(),
        Transaction::new(
            ReplicationPeerId([9; 32]),
            0,
            vec![Mutation::remove(OpaqueId(0), TestHealth::COMPONENT_ID)],
        ),
    )
    .expect("stale remove transaction buffers");

    app.update();

    assert!(
        app.world()
            .resource::<ReplicationFatalQueue>()
            .errors()
            .is_empty()
    );
    assert!(
        app.world()
            .resource::<EntityReplicas>()
            .entity(OpaqueId(0))
            .is_none()
    );
    assert_eq!(
        app.world()
            .resource::<AppliedTransactions>()
            .transactions()
            .len(),
        1
    );
}

#[test]
fn registered_component_completeness_assertion_allows_full_registration() {
    let mut app = test_app();

    app.update();
}

#[test]
#[should_panic(expected = "missing apply registration")]
fn registered_component_completeness_assertion_rejects_missing_apply() {
    let mut app = App::new();
    app.add_plugins(ReplicatedPlugin::<OpaqueId>::default());
    let engine_id = app.world_mut().register_component::<TestHealth>();
    app.world_mut()
        .resource_mut::<ComponentRegistry>()
        .register_for_assertion_test::<TestHealth>(engine_id);

    app.update();
}

#[test]
#[should_panic(expected = "missing engine-to-wire mapping")]
fn registered_component_completeness_assertion_rejects_missing_wire_mapping() {
    let mut app = App::new();
    app.add_plugins(ReplicatedPlugin::<OpaqueId>::default());
    let engine_id = app.world_mut().register_component::<TestHealth>();
    app.world_mut()
        .resource_mut::<ComponentRegistry>()
        .register_replicated::<TestHealth>(engine_id);

    app.update();
}

#[test]
#[should_panic(expected = "missing mediated encoder")]
fn registered_component_completeness_assertion_rejects_missing_encoder() {
    let mut app = App::new();
    app.add_plugins(ReplicatedPlugin::<OpaqueId>::default());
    let engine_id = app.world_mut().register_component::<TestHealth>();
    let mut registry = app.world_mut().resource_mut::<ComponentRegistry>();
    registry.register_replicated::<TestHealth>(engine_id);
    registry.map_engine(engine_id, TestHealth::COMPONENT_ID);

    app.update();
}

#[test]
fn drain_outbound_is_iterator() {
    let mut buffer = TransactionBuffer::<OpaqueId>::default();
    let _: std::collections::vec_deque::Drain<'_, Transaction<OpaqueId>> = buffer.drain_outbound();
}

#[derive(
    Replicated, Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[replicated(component_id = 99)]
#[rkyv(derive(Debug))]
struct DerivedReplicated(u32);

#[test]
fn replicated_derive_assigns_stable_id_and_mediated_component() {
    assert_eq!(DerivedReplicated::COMPONENT_ID, ComponentId(99));
    const { assert!(<DerivedReplicated as Component>::MEDIATED_ACCESS) };

    let encoded = DerivedReplicated(42).encode_payload().expect("encodes");
    let decoded = DerivedReplicated::decode_payload(&encoded).expect("decodes");

    assert_eq!(decoded, DerivedReplicated(42));
}

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(ReplicatedPlugin::<OpaqueId>::default());
    <App as ReplicatedAppExt<OpaqueId>>::register_replicated_component::<TestHealth>(&mut app);
    app
}

fn spawn_replicated_health(app: &mut App, value: u32) -> Entity {
    let entity = app.world_mut().spawn(TestHealth(value)).id();
    app.world_mut()
        .resource_mut::<EntityReplicas>()
        .bind(entity, OpaqueId(0));
    entity
}

fn health_payload(value: u32) -> Vec<u8> {
    TestHealth(value).encode_payload().expect("health encodes")
}
