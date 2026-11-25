use bevy::prelude::*;
use bevy_replicated::{
    AppliedTransactions, AuthorityOrdered, AuthorityOrderedEngine, AuthorityOrderedProposals,
    AuthorityPeer, CanonicalEventual, CanonicalEventualEngine, ComponentId, ConsistencyModel,
    EntityReplicas, Mutation, OpaqueId, ReliableOrdered, ReplicatedAppExt, ReplicatedComponent,
    ReplicatedPlugin, ReplicationChain, ReplicationPeerId, Transaction, WireError,
    receive_transaction,
};

const COMPONENT_ID: ComponentId = ComponentId(7_001);
const ENTITY_ID: OpaqueId = OpaqueId(42);
const AUTHORITY: ReplicationPeerId = ReplicationPeerId([1; 32]);
const READER: ReplicationPeerId = ReplicationPeerId([2; 32]);

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
struct ReplicatedValue(u32);

impl ReplicatedComponent for ReplicatedValue {
    const COMPONENT_ID: ComponentId = COMPONENT_ID;

    fn encode_payload(&self) -> Result<Vec<u8>, WireError> {
        rkyv::to_bytes::<rkyv::rancor::Error>(self)
            .map(|bytes| bytes.to_vec())
            .map_err(WireError::Encode)
    }

    fn decode_payload(bytes: &[u8]) -> Result<Self, WireError> {
        rkyv::from_bytes::<Self, rkyv::rancor::Error>(bytes).map_err(WireError::Decode)
    }
}

trait ModelExpectations: ConsistencyModel {
    const EXPECTED_APPLIED: usize;
    const EXPECTED_CHAIN_COUNT: u64;
    const EXPECTED_VALUE: u32;

    fn configure(app: &mut App);
    fn assert_model_specific(app: &App);
}

impl ModelExpectations for CanonicalEventual {
    const EXPECTED_APPLIED: usize = 2;
    const EXPECTED_CHAIN_COUNT: u64 = 2;
    const EXPECTED_VALUE: u32 = 20;

    fn configure(_app: &mut App) {}

    fn assert_model_specific(app: &App) {
        assert!(app.world().contains_resource::<CanonicalEventualEngine>());
        assert!(
            !app.world()
                .contains_resource::<AuthorityOrderedProposals<OpaqueId>>()
        );
    }
}

impl ModelExpectations for AuthorityOrdered {
    const EXPECTED_APPLIED: usize = 1;
    const EXPECTED_CHAIN_COUNT: u64 = 1;
    const EXPECTED_VALUE: u32 = 10;

    fn configure(app: &mut App) {
        app.world_mut()
            .insert_resource(AuthorityPeer::new(AUTHORITY));
    }

    fn assert_model_specific(app: &App) {
        assert!(app.world().contains_resource::<AuthorityOrderedEngine>());
        let proposals = app
            .world()
            .resource::<AuthorityOrderedProposals<OpaqueId>>()
            .proposals();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].peer_id, READER);
        assert!(
            app.world()
                .resource::<AppliedTransactions<OpaqueId>>()
                .transactions()
                .iter()
                .all(|transaction| transaction.peer_id == AUTHORITY)
        );
    }
}

#[test]
fn canonical_eventual_conforms_to_shared_scenario() {
    run_shared_scenario::<CanonicalEventual>();
}

#[test]
fn authority_ordered_conforms_to_shared_scenario() {
    run_shared_scenario::<AuthorityOrdered>();
}

fn run_shared_scenario<C: ModelExpectations>() {
    let (first_hash, first_value) = run_app::<C>(scenario_transactions());
    let (second_hash, second_value) = run_app::<C>(reversed_scenario_transactions());

    assert_eq!(first_hash, second_hash);
    assert_eq!(first_value, C::EXPECTED_VALUE);
    assert_eq!(second_value, C::EXPECTED_VALUE);
}

fn run_app<C: ModelExpectations>(
    transactions: Vec<Transaction<OpaqueId>>,
) -> (bevy_replicated::ChainHash, u32) {
    let mut app = App::new();
    app.add_plugins(ReplicatedPlugin::<OpaqueId, ReliableOrdered, C>::default());
    <App as ReplicatedAppExt<OpaqueId>>::register_replicated_component::<ReplicatedValue>(&mut app);
    C::configure(&mut app);

    let entity = app.world_mut().spawn(ReplicatedValue(0)).id();
    app.world_mut()
        .resource_mut::<EntityReplicas<OpaqueId>>()
        .bind(entity, ENTITY_ID);

    for transaction in transactions {
        receive_transaction(app.world_mut(), transaction).expect("transaction buffers");
    }
    app.update();

    assert_eq!(
        app.world()
            .resource::<AppliedTransactions<OpaqueId>>()
            .transactions()
            .len(),
        C::EXPECTED_APPLIED
    );
    assert_eq!(
        app.world().resource::<ReplicationChain>().count(),
        C::EXPECTED_CHAIN_COUNT
    );
    C::assert_model_specific(&app);

    let value = app
        .world()
        .get::<ReplicatedValue>(entity)
        .expect("replicated value exists")
        .0;
    (app.world().resource::<ReplicationChain>().current(), value)
}

fn scenario_transactions() -> Vec<Transaction<OpaqueId>> {
    vec![authority_transaction(), reader_transaction()]
}

fn reversed_scenario_transactions() -> Vec<Transaction<OpaqueId>> {
    vec![reader_transaction(), authority_transaction()]
}

fn authority_transaction() -> Transaction<OpaqueId> {
    Transaction::new(AUTHORITY, 0, vec![set_value(10)])
}

fn reader_transaction() -> Transaction<OpaqueId> {
    Transaction::new(READER, 0, vec![set_value(20)])
}

fn set_value(value: u32) -> Mutation<OpaqueId> {
    Mutation::modify(
        ENTITY_ID,
        COMPONENT_ID,
        ReplicatedValue(value)
            .encode_payload()
            .expect("value encodes"),
    )
}
