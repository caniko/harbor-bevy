use offline_action_log::{ActionId, ActionLogEntry, DeviceId, NegativeLamport};

fn entry(lamport: i64, id: u8) -> ActionLogEntry<u8> {
    let mut entry =
        ActionLogEntry::new(ActionId([id; 16]), lamport, DeviceId([0; 16]), -10, 1, 42).unwrap();
    entry.depends_on = Some(ActionId([0; 16]));
    entry.context_checksum = Some([2; 32]);
    entry.monotonic_elapsed_ms = Some(15);
    entry
}

#[test]
fn reconciliation_orders_by_lamport_then_action_id() {
    let mut entries = [entry(i64::MAX, 0), entry(1, 2), entry(1, 1), entry(0, 9)];
    entries.sort_by_key(ActionLogEntry::reconciliation_key);
    assert_eq!(
        entries.map(|entry| entry.reconciliation_key()),
        [
            (0, ActionId([9; 16])),
            (1, ActionId([1; 16])),
            (1, ActionId([2; 16])),
            (i64::MAX, ActionId([0; 16])),
        ]
    );
}

#[test]
fn construction_rejects_negative_lamport() {
    for negative in [-1, i64::MIN] {
        assert_eq!(
            ActionLogEntry::new(ActionId([0; 16]), negative, DeviceId([0; 16]), 0, 0, ()),
            Err(NegativeLamport(negative))
        );
    }
}

#[cfg(feature = "serde")]
#[test]
fn serde_preserves_fields_and_rejects_negative_wire_value() {
    let original = entry(7, 1);
    let mut wire = serde_json::to_value(&original).unwrap();
    assert_eq!(wire["lamport"], 7);
    assert_eq!(
        serde_json::from_value::<ActionLogEntry<u8>>(wire.clone()).unwrap(),
        original
    );
    wire["lamport"] = (-1).into();
    assert!(serde_json::from_value::<ActionLogEntry<u8>>(wire).is_err());
}

#[cfg(feature = "rkyv")]
#[test]
fn rkyv_round_trips_entry_and_ack() {
    use offline_action_log::{ActionAck, ActionAckStatus};
    use rkyv::rancor::Error;

    let original = (
        entry(i64::MAX, 1),
        ActionAck {
            action_id: ActionId([1; 16]),
            status: ActionAckStatus::RejectedRetryable,
        },
    );
    let bytes = rkyv::to_bytes::<Error>(&original).unwrap();
    assert_eq!(
        rkyv::from_bytes::<(ActionLogEntry<u8>, ActionAck), Error>(&bytes).unwrap(),
        original
    );
}
