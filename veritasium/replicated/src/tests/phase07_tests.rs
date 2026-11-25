use bevy::prelude::*;

use super::*;

const COMPONENT_ID: ComponentId = ComponentId(701);
const PEER_A: ReplicationPeerId = ReplicationPeerId([11; 32]);
const PEER_B: ReplicationPeerId = ReplicationPeerId([12; 32]);

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

#[derive(Resource, Default)]
struct Divergences(Vec<DivergenceEvent>);

#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum BoundaryState {
    #[default]
    Setup,
    Battle,
}

fn record_divergence(event: On<DivergenceEvent>, mut divergences: ResMut<Divergences>) {
    divergences.0.push(event.clone());
}

#[test]
fn chain_advances_on_apply() {
    let (mut app, _entity) = test_app(PEER_A);
    let genesis = app.world().resource::<ReplicationChain>().current();

    receive_transaction(app.world_mut(), tx(PEER_A, 0, 10)).expect("transaction buffers");
    app.update();

    let chain = app.world().resource::<ReplicationChain>();
    assert_eq!(chain.applied_count(), 1);
    assert_ne!(chain.current(), genesis);
    assert_eq!(chain.at_count(1), Some(chain.current()));
}

#[test]
fn two_peers_identical_transactions_same_hash() {
    let (mut left, _left_entity) = test_app(PEER_A);
    let (mut right, _right_entity) = test_app(PEER_B);
    let transactions = vec![
        tx(PEER_A, 0, 10),
        tx(PEER_A, 1, 11),
        tx(PEER_B, 0, 20),
        tx(PEER_B, 1, 21),
    ];

    deliver(&mut left, &transactions, &[3, 0, 2, 1]);
    deliver(&mut right, &transactions, &[2, 1, 3, 0]);
    left.update();
    right.update();

    assert_eq!(chain_hash(&left), chain_hash(&right));
    assert_eq!(chain_count(&left), 4);
    assert_eq!(chain_count(&right), 4);
}

#[test]
fn partial_arrival_ticks_still_apply_canonical_order() {
    let (mut left, _left_entity) = test_app(PEER_A);
    let (mut right, _right_entity) = test_app(PEER_B);
    configure_equal_peers(&mut left);
    configure_equal_peers(&mut right);
    let transactions = vec![
        tx(PEER_A, 0, 10),
        tx(PEER_A, 1, 11),
        tx(PEER_B, 0, 20),
        tx(PEER_B, 1, 21),
    ];

    deliver(&mut left, &transactions, &[0, 1, 2]);
    left.update();
    deliver(&mut right, &transactions, &[0, 2]);
    right.update();

    assert_eq!(
        applied_keys(&left),
        vec![
            TransactionKey::new(PEER_A, 0),
            TransactionKey::new(PEER_B, 0),
            TransactionKey::new(PEER_A, 1),
        ]
    );
    assert_eq!(
        applied_keys(&right),
        vec![
            TransactionKey::new(PEER_A, 0),
            TransactionKey::new(PEER_B, 0),
        ]
    );

    deliver(&mut right, &transactions, &[1]);
    right.update();
    assert_eq!(applied_keys(&left), applied_keys(&right));
    assert_eq!(chain_hash(&left), chain_hash(&right));

    deliver(&mut left, &transactions, &[3]);
    deliver(&mut right, &transactions, &[3]);
    left.update();
    right.update();

    assert_eq!(
        applied_keys(&left),
        vec![
            TransactionKey::new(PEER_A, 0),
            TransactionKey::new(PEER_B, 0),
            TransactionKey::new(PEER_A, 1),
            TransactionKey::new(PEER_B, 1),
        ]
    );
    assert_eq!(applied_keys(&left), applied_keys(&right));
    assert_eq!(chain_hash(&left), chain_hash(&right));
}

#[test]
fn canonical_ordering_gap_reports_blocking_key() {
    let (mut app, _entity) = test_app(PEER_A);
    configure_equal_peers(&mut app);
    app.world_mut().insert_resource(GapDetectionConfig {
        timeout: std::time::Duration::ZERO,
    });

    receive_transaction(app.world_mut(), tx(PEER_A, 0, 10)).expect("a0 buffers");
    receive_transaction(app.world_mut(), tx(PEER_A, 1, 11)).expect("a1 buffers");
    app.update();

    let alerts = app.world().resource::<TransactionGapAlerts>().alerts();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].peer_id, PEER_B);
    assert_eq!(alerts[0].expected_sequence, 0);
    assert_eq!(alerts[0].blocked_by, Some(TransactionKey::new(PEER_A, 1)));
}

#[test]
fn duplicate_transaction_key_is_idempotent_for_same_payload() {
    let (mut app, _entity) = test_app(PEER_A);
    let transaction = tx(PEER_A, 0, 10);

    assert_eq!(
        receive_transaction(app.world_mut(), transaction.clone()).expect("first buffers"),
        ReceiveTransactionStatus::Buffered
    );
    assert_eq!(
        receive_transaction(app.world_mut(), transaction.clone()).expect("duplicate buffers"),
        ReceiveTransactionStatus::DuplicateBuffered
    );
    app.update();
    assert_eq!(
        receive_transaction(app.world_mut(), transaction).expect("duplicate applied"),
        ReceiveTransactionStatus::DuplicateApplied
    );
}

#[test]
fn divergent_payload_with_same_key_is_rejected() {
    let (mut app, _entity) = test_app(PEER_A);
    receive_transaction(app.world_mut(), tx(PEER_A, 0, 10)).expect("first buffers");

    let error = receive_transaction(app.world_mut(), tx(PEER_A, 0, 11))
        .expect_err("conflicting payload rejects");
    let message = error.to_string();
    assert!(message.contains("reused key"), "{message}");
    assert!(message.contains("buffered"), "{message}");

    app.update();
    let error = receive_transaction(app.world_mut(), tx(PEER_A, 0, 12))
        .expect_err("conflicting applied payload rejects");
    let message = error.to_string();
    assert!(message.contains("applied"), "{message}");
}

#[test]
fn chain_diverges_on_different_transactions() {
    let mut left = ReplicationChain::new();
    let mut right = ReplicationChain::new();

    left.fold(&tx(PEER_A, 0, 10)).expect("left folds");
    right.fold(&tx(PEER_A, 0, 11)).expect("right folds");

    assert_ne!(left.current(), right.current());
    assert_eq!(left.applied_count(), right.applied_count());
}

#[test]
fn divergence_event_fires_on_mismatch_report() {
    let (mut app, _entity) = test_app(PEER_A);
    app.init_resource::<Divergences>();
    app.add_observer(record_divergence);

    let matching_genesis = app.world().resource::<ReplicationChain>().current();
    let matched = receive_chain_hash_report(
        app.world_mut(),
        ChainHashReport::new(PEER_B, 0, matching_genesis),
    );
    assert_eq!(matched, ChainHashReportStatus::Matched);

    receive_transaction(app.world_mut(), tx(PEER_A, 0, 10)).expect("transaction buffers");
    app.update();

    let mut remote = ReplicationChain::new();
    remote.fold(&tx(PEER_A, 0, 11)).expect("remote folds");
    let status = receive_chain_hash_report(
        app.world_mut(),
        ChainHashReport::new(PEER_B, remote.applied_count(), remote.current()),
    );

    assert_eq!(status, ChainHashReportStatus::Mismatched);
    let divergences = &app.world().resource::<Divergences>().0;
    assert_eq!(divergences.len(), 1);
    assert_eq!(divergences[0].remote_peer, PEER_B);
    assert_eq!(divergences[0].applied_count_at_check, 1);
    assert_eq!(divergences[0].last_common_count, Some(0));
    assert_eq!(
        divergences[0].local_hash,
        app.world().resource::<ReplicationChain>().current()
    );
    assert_eq!(divergences[0].remote_hash, remote.current());
}

#[test]
fn duplicate_mismatch_report_is_suppressed() {
    let (mut app, _entity) = test_app(PEER_A);
    app.init_resource::<Divergences>();
    app.add_observer(record_divergence);

    receive_transaction(app.world_mut(), tx(PEER_A, 0, 10)).expect("transaction buffers");
    app.update();

    let mut remote = ReplicationChain::new();
    remote.fold(&tx(PEER_A, 0, 11)).expect("remote folds");
    let report = ChainHashReport::new(PEER_B, remote.applied_count(), remote.current());

    assert_eq!(
        receive_chain_hash_report(app.world_mut(), report),
        ChainHashReportStatus::Mismatched
    );
    assert_eq!(
        receive_chain_hash_report(app.world_mut(), report),
        ChainHashReportStatus::Mismatched
    );
    assert_eq!(app.world().resource::<Divergences>().0.len(), 1);
}

#[test]
fn chain_hash_serializes_deterministically() {
    let report = ChainHashReport::new(PEER_B, 42, [9; 32]);

    let bytes = encode_chain_hash_report(&report).expect("report encodes");
    let decoded = decode_chain_hash_report(&bytes).expect("report decodes");
    let encoded_again = encode_chain_hash_report(&decoded).expect("report re-encodes");

    assert_eq!(decoded, report);
    assert_eq!(encoded_again, bytes);
}

#[test]
fn chain_hash_domain_is_bevy_replicated_transaction_domain() {
    assert_eq!(CHAIN_DOMAIN_TAG, b"bevy_replicated.transaction_chain.v3");
}

#[test]
fn chain_checkpoint_report_round_trips_between_peers() {
    let (mut left, _left_entity) = test_app(PEER_A);
    let (mut right, _right_entity) = test_app(PEER_B);
    let transaction = tx(PEER_A, 0, 10);

    receive_transaction(left.world_mut(), transaction.clone()).expect("left buffers");
    receive_transaction(right.world_mut(), transaction).expect("right buffers");
    left.update();
    right.update();

    request_chain_hash_report(left.world_mut()).expect("left report requested");
    let envelope = left
        .world_mut()
        .resource_mut::<ChainHashOutbox>()
        .drain()
        .pop()
        .expect("report emitted");
    let decoded = decode_chain_hash_report(&envelope.wire_bytes).expect("report decodes");

    assert_eq!(decoded, envelope.report);
    assert_eq!(
        receive_chain_hash_report(right.world_mut(), decoded),
        ChainHashReportStatus::Matched
    );
}

#[test]
fn default_cadence_reports_after_configured_transaction_count() {
    let (mut app, _entity) = test_app(PEER_A);
    app.world_mut().insert_resource(ChainHashConfig {
        transaction_cadence: 2,
        report_on_phase_boundary: true,
    });

    receive_transaction(app.world_mut(), tx(PEER_A, 0, 10)).expect("transaction buffers");
    receive_transaction(app.world_mut(), tx(PEER_A, 1, 11)).expect("transaction buffers");
    app.update();

    let outbox = app.world().resource::<ChainHashOutbox>();
    assert_eq!(outbox.envelopes().len(), 1);
    assert_eq!(outbox.envelopes()[0].report.peer(), PEER_A);
    assert_eq!(outbox.envelopes()[0].report.applied_count(), Some(2));
    assert_eq!(outbox.envelopes()[0].report.hash(), Some(chain_hash(&app)));
}

#[test]
fn chain_history_report_emits_evicted_variant() {
    let (mut app, _entity) = test_app_with_capacity(PEER_A, ChainHistoryCapacity(4));
    app.world_mut().insert_resource(ChainHashConfig {
        transaction_cadence: 1,
        report_on_phase_boundary: true,
    });

    for sequence in 0..6 {
        receive_transaction(app.world_mut(), tx(PEER_A, sequence, 10 + sequence as u32))
            .expect("transaction buffers");
    }
    app.update();
    app.update();

    let outbox = app.world().resource::<ChainHashOutbox>();
    assert!(matches!(
        outbox.envelopes()[0].report,
        ChainHashReport::HistoryEvicted {
            peer: PEER_A,
            requested_count: 1,
            oldest_available: 2,
        }
    ));
    assert!(
        outbox
            .envelopes()
            .iter()
            .any(|envelope| envelope.report.applied_count() == Some(2))
    );
}

#[test]
fn phase_boundary_state_transition_emits_report() {
    let mut app = App::new();
    app.add_plugins(bevy::state::app::StatesPlugin)
        .add_plugins(ReplicatedPlugin::<OpaqueId>::default())
        .init_state::<BoundaryState>();
    <App as ReplicatedAppExt<OpaqueId>>::exchange_chain_hash_on_state_transition::<BoundaryState>(
        &mut app,
    );
    app.world_mut()
        .resource_mut::<ReplicationIdentity>()
        .peer_id = PEER_A;

    app.update();
    app.world_mut().resource_mut::<ChainHashOutbox>().drain();
    app.world_mut()
        .resource_mut::<NextState<BoundaryState>>()
        .set(BoundaryState::Battle);
    app.update();

    let outbox = app.world().resource::<ChainHashOutbox>();
    assert_eq!(outbox.envelopes().len(), 1);
    assert_eq!(outbox.envelopes()[0].report.peer(), PEER_A);
    assert_eq!(outbox.envelopes()[0].report.applied_count(), Some(0));
    assert_eq!(outbox.envelopes()[0].report.hash(), Some(chain_hash(&app)));
}

fn test_app(peer_id: ReplicationPeerId) -> (App, Entity) {
    test_app_with_capacity(peer_id, ChainHistoryCapacity::default())
}

fn test_app_with_capacity(
    peer_id: ReplicationPeerId,
    capacity: ChainHistoryCapacity,
) -> (App, Entity) {
    let mut app = App::new();
    app.add_plugins(ReplicatedPlugin::<OpaqueId>::with_chain_history_capacity(
        capacity,
    ));
    <App as ReplicatedAppExt<OpaqueId>>::register_replicated_component::<ReplicatedValue>(&mut app);
    app.world_mut()
        .resource_mut::<ReplicationIdentity>()
        .peer_id = peer_id;
    app.world_mut()
        .resource_mut::<TransactionBuffer>()
        .set_peer_id(peer_id);
    let entity = app.world_mut().spawn(ReplicatedValue(0)).id();
    app.world_mut()
        .resource_mut::<EntityReplicas>()
        .bind(entity, OpaqueId(0));
    (app, entity)
}

fn configure_equal_peers(app: &mut App) {
    app.world_mut()
        .resource_mut::<ReplicationParticipants>()
        .set([PEER_A, PEER_B]);
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

fn deliver(app: &mut App, transactions: &[Transaction], order: &[usize]) {
    for &index in order {
        receive_transaction(app.world_mut(), transactions[index].clone())
            .expect("transaction buffers");
    }
}

fn chain_hash(app: &App) -> ChainHash {
    app.world().resource::<ReplicationChain>().current()
}

fn chain_count(app: &App) -> u64 {
    app.world().resource::<ReplicationChain>().applied_count()
}

fn applied_keys(app: &App) -> Vec<TransactionKey> {
    app.world()
        .resource::<AppliedTransactions>()
        .transactions()
        .iter()
        .map(|transaction| TransactionKey::from_transaction(transaction.as_ref()))
        .collect()
}
