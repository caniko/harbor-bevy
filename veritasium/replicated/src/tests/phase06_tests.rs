use std::time::Duration;

use bevy::prelude::*;
use proptest::prelude::*;

use super::*;

const COMPONENT_ID: ComponentId = ComponentId(501);
const PEER_A: ReplicationPeerId = ReplicationPeerId([1; 32]);
const PEER_B: ReplicationPeerId = ReplicationPeerId([2; 32]);

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

#[test]
fn out_of_order_receive_buffers_correctly() {
    let (mut app, entity) = test_app();

    receive_transaction(app.world_mut(), tx(PEER_A, 1, 20)).expect("seq 1 buffers");
    app.update();

    assert_eq!(value(&app, entity), 0);
    assert!(applied_keys(&app).is_empty());

    receive_transaction(app.world_mut(), tx(PEER_A, 0, 10)).expect("seq 0 buffers");
    app.update();

    assert_eq!(value(&app, entity), 20);
    assert_eq!(
        applied_keys(&app),
        vec![
            TransactionKey::new(PEER_A, 0),
            TransactionKey::new(PEER_A, 1)
        ]
    );
    assert_eq!(app.world().resource::<ReplicationChain>().count(), 2);
}

#[test]
fn gap_detection_alerts() {
    let (mut app, _entity) = test_app();
    app.world_mut().insert_resource(GapDetectionConfig {
        timeout: Duration::ZERO,
    });

    receive_transaction(app.world_mut(), tx(PEER_A, 2, 20)).expect("future tx buffers");
    app.update();

    let alerts = app.world().resource::<TransactionGapAlerts>().alerts();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].peer_id, PEER_A);
    assert_eq!(alerts[0].expected_sequence, 0);
    assert_eq!(alerts[0].first_observed_sequence, 2);
    assert_eq!(alerts[0].buffered_count, 1);
    assert_eq!(alerts[0].blocked_by, None);
}

#[test]
fn two_peers_applying_same_set_produce_identical_state() {
    let (mut left, left_entity) = test_app();
    let (mut right, right_entity) = test_app();
    let transactions = vec![
        tx(PEER_A, 0, 10),
        tx(PEER_A, 1, 11),
        tx(PEER_B, 0, 20),
        tx(PEER_B, 1, 21),
    ];

    deliver(&mut left, &transactions, &[3, 1, 2, 0]);
    deliver(&mut right, &transactions, &[2, 0, 3, 1]);
    left.update();
    right.update();

    assert_eq!(value(&left, left_entity), value(&right, right_entity));
    assert_eq!(applied_keys(&left), applied_keys(&right));
    assert_eq!(
        applied_keys(&left),
        vec![
            TransactionKey::new(PEER_A, 0),
            TransactionKey::new(PEER_B, 0),
            TransactionKey::new(PEER_A, 1),
            TransactionKey::new(PEER_B, 1)
        ]
    );
}

proptest! {
    #[test]
    fn randomized_transaction_interleavings_converge(
        a_count in 0usize..8,
        b_count in 0usize..8,
        left_weights in proptest::collection::vec(any::<u8>(), 0..16),
        right_weights in proptest::collection::vec(any::<u8>(), 0..16),
        duplicate_weights in proptest::collection::vec(any::<u8>(), 0..16),
    ) {
        let transactions = transaction_set(a_count, b_count);
        let left_order = weighted_order(transactions.len(), &left_weights);
        let right_order = weighted_order(transactions.len(), &right_weights);
        let (mut left, left_entity) = test_app();
        let (mut right, right_entity) = test_app();

        deliver_with_duplicates(&mut left, &transactions, &left_order, &duplicate_weights);
        deliver_with_duplicates(&mut right, &transactions, &right_order, &duplicate_weights);
        left.update();
        right.update();

        prop_assert_eq!(value(&left, left_entity), value(&right, right_entity));
        prop_assert_eq!(applied_keys(&left), applied_keys(&right));
        prop_assert_eq!(
            app_chain(&left),
            app_chain(&right)
        );
    }
}

fn test_app() -> (App, Entity) {
    let mut app = App::new();
    app.add_plugins(ReplicatedPlugin::<OpaqueId>::default());
    <App as ReplicatedAppExt<OpaqueId>>::register_replicated_component::<ReplicatedValue>(&mut app);
    let entity = app.world_mut().spawn(ReplicatedValue(0)).id();
    app.world_mut()
        .resource_mut::<EntityReplicas>()
        .bind(entity, OpaqueId(0));
    (app, entity)
}

fn tx(peer_id: ReplicationPeerId, sequence: u64, value: u32) -> Transaction {
    Transaction::new(
        peer_id,
        sequence,
        vec![Mutation::modify(
            OpaqueId(0),
            COMPONENT_ID,
            ReplicatedValue(value).encode_payload().unwrap(),
        )],
    )
}

fn transaction_set(a_count: usize, b_count: usize) -> Vec<Transaction> {
    let mut transactions = Vec::new();
    for sequence in 0..a_count {
        transactions.push(tx(PEER_A, sequence as u64, 100 + sequence as u32));
    }
    for sequence in 0..b_count {
        transactions.push(tx(PEER_B, sequence as u64, 200 + sequence as u32));
    }
    transactions
}

fn deliver(app: &mut App, transactions: &[Transaction], order: &[usize]) {
    for &index in order {
        receive_transaction(app.world_mut(), transactions[index].clone())
            .expect("transaction buffers");
    }
}

fn deliver_with_duplicates(
    app: &mut App,
    transactions: &[Transaction],
    order: &[usize],
    duplicate_weights: &[u8],
) {
    for &index in order {
        receive_transaction(app.world_mut(), transactions[index].clone())
            .expect("transaction buffers");
        if duplicate_weights.get(index).copied().unwrap_or(1) % 5 == 0 {
            receive_transaction(app.world_mut(), transactions[index].clone())
                .expect("duplicate delivery is idempotent");
        }
    }
}

fn weighted_order(len: usize, weights: &[u8]) -> Vec<usize> {
    let mut order: Vec<_> = (0..len).collect();
    order.sort_by_key(|index| {
        (
            weights
                .get(*index)
                .copied()
                .unwrap_or((*index as u8).wrapping_mul(37)),
            *index,
        )
    });
    order
}

fn value(app: &App, entity: Entity) -> u32 {
    app.world().get::<ReplicatedValue>(entity).unwrap().0
}

fn applied_keys(app: &App) -> Vec<TransactionKey> {
    app.world()
        .resource::<AppliedTransactions>()
        .transactions()
        .iter()
        .map(|transaction| TransactionKey::from_transaction(transaction.as_ref()))
        .collect()
}

fn app_chain(app: &App) -> [u8; 32] {
    app.world().resource::<ReplicationChain>().current()
}
