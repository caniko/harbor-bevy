use bevy::prelude::*;
use bevy_replicated::ReliableOrdered;
use bevy_replicated::interop::{
    ChannelKind, ChannelSupport, ReplicationPacketKind, drain_world_outbound_packets,
};
use bevy_replicated::{
    AppliedTransactions, ChainHashReport, ComponentId, EntityReplicas, GossipOutbox, Mutation,
    OpaqueId, Replicated, ReplicatedAppExt, ReplicatedComponent, ReplicatedPlugin,
    ReplicatedResMut, ReplicatedResource, ReplicationChain, ReplicationError,
    ReplicationFatalQueue, ReplicationIdentity, ReplicationPeerId, ResourceId, ResourceRegistry,
    Transaction, TransactionBuffer, decode_chain_hash_report, decode_transaction,
    encode_chain_hash_report, insert_replicated_resource_as, receive_transaction,
    remove_replicated_resource_as,
};

const COMPONENT_ID: ComponentId = ComponentId(70_201);
const RESOURCE_ID: ResourceId = ResourceId(70_202);
const ENTITY_ID: OpaqueId = OpaqueId::new(42);
const SOURCE_PEER: ReplicationPeerId = ReplicationPeerId([8; 32]);
const TARGET_PEER: ReplicationPeerId = ReplicationPeerId([9; 32]);

#[derive(
    Replicated, Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[replicated(component_id = 70201)]
struct PublicValue(u32);

#[derive(
    ReplicatedResource,
    Debug,
    Clone,
    PartialEq,
    Eq,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[replicated(resource_id = 70202)]
struct PublicCounter(u32);

#[derive(
    ReplicatedResource,
    Debug,
    Clone,
    PartialEq,
    Eq,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[replicated(resource_id = 70202)]
struct DuplicatePublicCounter(u32);

#[derive(Resource, Default)]
struct PreviousCounter(Option<PublicCounter>);

#[test]
fn public_reexports_commit_encode_decode_and_apply_transaction() {
    let mut source = public_app(SOURCE_PEER);
    let source_entity = spawn_bound_value(&mut source, 1);
    source.update();

    let payload = PublicValue(9).encode_payload().expect("value encodes");
    source
        .world_mut()
        .resource_mut::<TransactionBuffer<OpaqueId>>()
        .record(Mutation::modify(ENTITY_ID, COMPONENT_ID, payload));

    bevy_replicated::commit_now(source.world_mut()).expect("source transaction commits");

    assert_eq!(
        source.world().get::<PublicValue>(source_entity),
        Some(&PublicValue(1))
    );
    assert_eq!(
        source
            .world()
            .resource::<AppliedTransactions<OpaqueId>>()
            .transactions()
            .len(),
        1
    );

    let envelope = source
        .world()
        .resource::<GossipOutbox<OpaqueId>>()
        .envelopes()
        .first()
        .expect("committed transaction is exposed for transport");
    let decoded = decode_transaction::<OpaqueId>(&envelope.wire_bytes)
        .expect("public wire decoder accepts public encoder bytes");
    assert_eq!(decoded, *envelope.transaction);

    let mut target = public_app(TARGET_PEER);
    let target_entity = spawn_bound_value(&mut target, 0);
    target.update();
    receive_transaction(target.world_mut(), decoded).expect("target buffers transaction");
    target.update();

    assert_eq!(
        target.world().get::<PublicValue>(target_entity),
        Some(&PublicValue(9))
    );
    assert_eq!(
        source.world().resource::<ReplicationChain>().current(),
        target.world().resource::<ReplicationChain>().current()
    );
}

#[test]
fn public_gossip_outbox_drain_preserves_committed_wire_bytes() {
    let mut app = public_app(SOURCE_PEER);
    spawn_bound_value(&mut app, 1);
    app.update();

    let payload = PublicValue(11).encode_payload().expect("value encodes");
    app.world_mut()
        .resource_mut::<TransactionBuffer<OpaqueId>>()
        .record(Mutation::modify(ENTITY_ID, COMPONENT_ID, payload));
    bevy_replicated::commit_now(app.world_mut()).expect("transaction commits");

    let drained = drain_world_outbound_packets::<OpaqueId, ReliableOrdered>(
        app.world_mut(),
        ChannelSupport::reliable_ordered_only(),
    )
    .expect("world packet drain succeeds");
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].channel, ChannelKind::ReliableOrdered);
    assert_eq!(drained[0].kind, ReplicationPacketKind::Transaction);
    assert_eq!(
        decode_transaction::<OpaqueId>(&drained[0].payload).expect("wire bytes decode"),
        Transaction::new(
            SOURCE_PEER,
            0,
            vec![Mutation::modify(
                ENTITY_ID,
                COMPONENT_ID,
                PublicValue(11).encode_payload().expect("value encodes")
            )]
        )
    );
}

#[test]
fn public_replicated_component_contract_encodes_decodes_and_merges() {
    let value = PublicValue(13);
    let payload = value.encode_payload().expect("value encodes");

    assert_eq!(PublicValue::COMPONENT_ID, COMPONENT_ID);
    assert_eq!(
        PublicValue::decode_payload(&payload).expect("payload decodes"),
        value
    );
    assert_eq!(
        PublicValue::merge_remote(Some(&PublicValue(1)), PublicValue(2)),
        PublicValue(2)
    );
}

#[test]
fn public_replicated_resource_contract_encodes_decodes_and_merges() {
    let value = PublicCounter(13);
    let payload = value.encode_payload().expect("resource encodes");

    assert_eq!(PublicCounter::RESOURCE_ID, RESOURCE_ID);
    assert_eq!(
        PublicCounter::decode_payload(&payload).expect("payload decodes"),
        value
    );
    assert_eq!(
        PublicCounter::merge_remote(Some(&PublicCounter(1)), PublicCounter(2)),
        PublicCounter(2)
    );
}

#[test]
fn public_replicated_resource_mutation_converges_through_interop_packets() {
    let mut source = public_app(SOURCE_PEER);
    source.world_mut().insert_resource(PublicCounter(1));
    source.add_systems(
        Update,
        increment_counter.run_if(bevy::ecs::schedule::common_conditions::run_once),
    );

    source.update();
    let packet = drain_world_outbound_packets::<OpaqueId, ReliableOrdered>(
        source.world_mut(),
        ChannelSupport::reliable_ordered_only(),
    )
    .expect("packet drain succeeds")
    .pop()
    .expect("resource mutation packet exists");
    source.update();

    assert_eq!(
        source.world().resource::<PublicCounter>(),
        &PublicCounter(7)
    );
    assert_eq!(packet.kind, ReplicationPacketKind::Transaction);

    let mut target = public_app(TARGET_PEER);
    target.world_mut().insert_resource(PublicCounter(0));
    let decoded =
        decode_transaction::<OpaqueId>(&packet.payload).expect("resource transaction decodes");
    receive_transaction(target.world_mut(), decoded).expect("target buffers resource transaction");
    target.update();

    assert_eq!(
        target.world().resource::<PublicCounter>(),
        &PublicCounter(7)
    );
    assert_eq!(
        source.world().resource::<ReplicationChain>().current(),
        target.world().resource::<ReplicationChain>().current()
    );
}

#[test]
fn public_replicated_resource_registry_catalog_is_auditable_and_idempotent() {
    let mut app = public_app(SOURCE_PEER);
    <App as ReplicatedAppExt<OpaqueId>>::register_replicated_resource::<PublicCounter>(&mut app);

    let registered = app
        .world()
        .resource::<ResourceRegistry<OpaqueId>>()
        .registered();
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].resource_id, RESOURCE_ID);
    assert_eq!(
        registered[0].type_name,
        std::any::type_name::<PublicCounter>()
    );
}

#[test]
#[should_panic(expected = "duplicate ResourceId")]
fn public_replicated_resource_registry_rejects_duplicate_resource_ids() {
    let mut app = public_app(SOURCE_PEER);
    <App as ReplicatedAppExt<OpaqueId>>::register_replicated_resource::<DuplicatePublicCounter>(
        &mut app,
    );
}

#[test]
fn public_replicated_resource_replace_records_modify_and_converges() {
    let mut source = public_app(SOURCE_PEER);
    source.world_mut().insert_resource(PublicCounter(1));
    source.init_resource::<PreviousCounter>();
    source.add_systems(
        Update,
        replace_counter.run_if(bevy::ecs::schedule::common_conditions::run_once),
    );

    source.update();
    let packet = drain_world_outbound_packets::<OpaqueId, ReliableOrdered>(
        source.world_mut(),
        ChannelSupport::reliable_ordered_only(),
    )
    .expect("packet drain succeeds")
    .pop()
    .expect("resource replace packet exists");
    source.update();

    assert_eq!(
        source.world().resource::<PreviousCounter>().0,
        Some(PublicCounter(1))
    );
    assert_eq!(
        source.world().resource::<PublicCounter>(),
        &PublicCounter(21)
    );

    let mut target = public_app(TARGET_PEER);
    target.world_mut().insert_resource(PublicCounter(0));
    let decoded =
        decode_transaction::<OpaqueId>(&packet.payload).expect("resource transaction decodes");
    assert_eq!(decoded.mutations.len(), 1);
    assert_eq!(
        decoded.mutations[0],
        Mutation::modify_resource(
            RESOURCE_ID,
            PublicCounter(21)
                .encode_payload()
                .expect("resource encodes")
        )
    );

    receive_transaction(target.world_mut(), decoded).expect("target buffers resource transaction");
    target.update();

    assert_eq!(
        target.world().resource::<PublicCounter>(),
        &PublicCounter(21)
    );
    assert_eq!(
        source.world().resource::<ReplicationChain>().current(),
        target.world().resource::<ReplicationChain>().current()
    );
}

#[test]
fn public_replicated_resource_record_current_records_current_post_image() {
    let mut app = public_app(SOURCE_PEER);
    app.world_mut().insert_resource(PublicCounter(3));
    app.add_systems(
        Update,
        (set_counter_with_plain_res_mut, record_current_counter)
            .chain()
            .run_if(bevy::ecs::schedule::common_conditions::run_once),
    );

    app.update();
    let packet = drain_world_outbound_packets::<OpaqueId, ReliableOrdered>(
        app.world_mut(),
        ChannelSupport::reliable_ordered_only(),
    )
    .expect("packet drain succeeds")
    .pop()
    .expect("resource record-current packet exists");

    let decoded =
        decode_transaction::<OpaqueId>(&packet.payload).expect("resource transaction decodes");
    assert_eq!(
        decoded.mutations,
        vec![Mutation::modify_resource(
            RESOURCE_ID,
            PublicCounter(17)
                .encode_payload()
                .expect("resource encodes")
        )]
    );
}

#[test]
fn public_unknown_resource_transaction_surfaces_in_fatal_queue() {
    let mut app = public_app(TARGET_PEER);
    let transaction: Transaction<OpaqueId> = Transaction::new(
        SOURCE_PEER,
        0,
        vec![Mutation::modify_resource(
            ResourceId(999_999),
            vec![1, 2, 3],
        )],
    );

    receive_transaction(app.world_mut(), transaction)
        .expect("unknown resource transaction buffers");
    app.update();

    let errors = app
        .world()
        .resource::<ReplicationFatalQueue<OpaqueId>>()
        .errors();
    assert_eq!(errors.len(), 1);
    assert!(matches!(
        errors[0].error,
        ReplicationError::UnknownResource(ResourceId(999_999))
    ));
}

#[test]
fn public_replicated_resource_insert_and_remove_helpers_emit_resource_mutations() {
    let mut app = public_app(SOURCE_PEER);

    insert_replicated_resource_as::<OpaqueId, PublicCounter>(app.world_mut(), PublicCounter(12))
        .expect("resource insert records");
    bevy_replicated::commit_now_as::<OpaqueId>(app.world_mut()).expect("insert commits");

    assert_eq!(app.world().resource::<PublicCounter>(), &PublicCounter(12));
    assert_eq!(
        app.world().resource::<GossipOutbox<OpaqueId>>().envelopes()[0]
            .transaction
            .mutations[0],
        Mutation::insert_resource(
            RESOURCE_ID,
            PublicCounter(12)
                .encode_payload()
                .expect("resource encodes")
        )
    );

    let removed = remove_replicated_resource_as::<OpaqueId, PublicCounter>(app.world_mut())
        .expect("resource remove records");
    assert_eq!(removed, Some(PublicCounter(12)));
    bevy_replicated::commit_now_as::<OpaqueId>(app.world_mut()).expect("remove commits");

    assert!(
        app.world()
            .resource::<GossipOutbox<OpaqueId>>()
            .envelopes()
            .iter()
            .any(|envelope| envelope.transaction.mutations
                == vec![Mutation::remove_resource(RESOURCE_ID)])
    );
}

#[test]
fn public_chain_hash_report_wire_round_trips() {
    let report = ChainHashReport::new(SOURCE_PEER, 4, [0x2a; 32]);
    let bytes = encode_chain_hash_report(&report).expect("chain hash report encodes");

    let decoded = decode_chain_hash_report(&bytes).expect("chain hash report decodes");

    assert_eq!(decoded, report);
    assert_eq!(decoded.peer(), SOURCE_PEER);
    assert_eq!(decoded.applied_count(), Some(4));
    assert_eq!(decoded.hash(), Some([0x2a; 32]));
}

#[test]
fn opaque_id_constructor_is_const_and_readable() {
    const ID: OpaqueId = OpaqueId::new(77);
    assert_eq!(ID.get(), 77);
}

fn public_app(peer_id: ReplicationPeerId) -> App {
    let mut app = App::new();
    app.add_plugins(ReplicatedPlugin::<OpaqueId>::default());
    <App as ReplicatedAppExt<OpaqueId>>::register_replicated_component::<PublicValue>(&mut app);
    <App as ReplicatedAppExt<OpaqueId>>::register_replicated_resource::<PublicCounter>(&mut app);
    app.world_mut()
        .resource_mut::<ReplicationIdentity>()
        .peer_id = peer_id;
    app.world_mut()
        .resource_mut::<TransactionBuffer<OpaqueId>>()
        .set_peer_id(peer_id);
    app
}

fn increment_counter(mut counter: ReplicatedResMut<PublicCounter, OpaqueId>) {
    counter
        .modify(|counter| counter.0 = 7)
        .expect("resource mutation records");
}

fn replace_counter(
    mut counter: ReplicatedResMut<PublicCounter, OpaqueId>,
    mut previous: ResMut<PreviousCounter>,
) {
    previous.0 = Some(
        counter
            .replace(PublicCounter(21))
            .expect("resource replace records"),
    );
}

fn set_counter_with_plain_res_mut(mut counter: ResMut<PublicCounter>) {
    counter.0 = 17;
}

fn record_current_counter(mut counter: ReplicatedResMut<PublicCounter, OpaqueId>) {
    counter
        .record_current()
        .expect("current resource post-image records");
}

fn spawn_bound_value(app: &mut App, value: u32) -> Entity {
    let entity = app.world_mut().spawn(PublicValue(value)).id();
    app.world_mut()
        .resource_mut::<EntityReplicas<OpaqueId>>()
        .bind(entity, ENTITY_ID);
    entity
}
