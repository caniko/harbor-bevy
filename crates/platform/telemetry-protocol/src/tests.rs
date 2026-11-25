use super::{Batch, ClientHeader, Envelope, EventMetadata, SessionId, Timestamp};

#[derive(Debug, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
enum Event {
    Started,
    Measured { key: String, value: i64 },
}

fn envelope<E>(event: E, millis: u64) -> Envelope<E> {
    Envelope {
        metadata: EventMetadata {
            timestamp: Timestamp(millis),
            session: SessionId("session-1".into()),
            client: ClientHeader {
                name: "demo".into(),
                version: "1.0.0".into(),
            },
        },
        event,
    }
}

#[test]
fn round_trips_caller_defined_event_and_metadata() {
    let envelope = envelope(
        Event::Measured {
            key: "duration".into(),
            value: -42,
        },
        1_725_000_001_234,
    );
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope).unwrap();
    assert_eq!(
        rkyv::from_bytes::<Envelope<Event>, rkyv::rancor::Error>(&bytes).unwrap(),
        envelope
    );
}

#[test]
fn batch_preserves_event_order_and_can_be_empty() {
    for events in [
        vec![],
        vec![
            envelope(Event::Started, 1),
            envelope(
                Event::Measured {
                    key: "count".into(),
                    value: 9,
                },
                2,
            ),
        ],
    ] {
        let batch = Batch { events };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&batch).unwrap();
        assert_eq!(
            rkyv::from_bytes::<Batch<Event>, rkyv::rancor::Error>(&bytes).unwrap(),
            batch
        );
    }
}

#[test]
fn second_payload_type_uses_the_same_envelope_and_truncated_data_is_rejected() {
    let envelope = envelope(String::from("hello"), 0);
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope).unwrap();
    assert_eq!(
        rkyv::from_bytes::<Envelope<String>, rkyv::rancor::Error>(&bytes).unwrap(),
        envelope
    );
    assert!(rkyv::from_bytes::<Envelope<String>, rkyv::rancor::Error>(&bytes[..3]).is_err());
}
