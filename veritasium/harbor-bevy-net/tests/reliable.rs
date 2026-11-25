use bevy_replicated::epoch::SessionEpoch;
use harbor_bevy_net::*;
use std::time::{Duration, Instant};
use veritasium_wire::PlayerId;

fn context() -> MatchContext {
    MatchContext {
        protocol_version: 1,
        run_id: [1; 16],
        match_id: [2; 16],
        session_epoch: 1,
        connection_epoch: 1,
        sender: PlayerId::ONE,
    }
}

fn send(sender: &mut ReliableSender, now: Instant, payload: u8) -> MatchEnvelope {
    let mut epoch = SessionEpoch::SENTINEL;
    epoch.match_id = context().match_id;
    sender
        .send(ProtocolStream::Session, epoch, 0, vec![payload], now)
        .unwrap()
}

#[test]
fn reordering_preserves_both_messages_and_dedupe_detects_conflicts() {
    let now = Instant::now();
    let mut sender = ReliableSender::new(context());
    let one = send(&mut sender, now, 1);
    let two = send(&mut sender, now, 2);
    let mut reorder = ReorderBuffer::new(PlayerId::ONE, ProtocolStream::Session);
    let mut dedupe = DedupeCache::default();
    assert!(reorder.push(two.clone()).unwrap().is_empty());
    assert!(reorder.push(two.clone()).unwrap().is_empty());
    let ready = reorder.push(one.clone()).unwrap();
    assert_eq!(
        ready.iter().map(|packet| packet.seq).collect::<Vec<_>>(),
        [1, 2]
    );
    for packet in &ready {
        assert!(dedupe.try_accept(packet).unwrap());
    }
    assert!(!dedupe.try_accept(&one).unwrap());
    let mut conflict = one;
    conflict.payload = vec![3];
    assert_eq!(
        dedupe.try_accept(&conflict),
        Err(SessionFault::PayloadDigestMismatch)
    );
    conflict.payload_hash = *blake3::hash(&conflict.payload).as_bytes();
    assert_eq!(
        dedupe.try_accept(&conflict),
        Err(SessionFault::ConflictingDuplicate)
    );
}

#[test]
fn ack_selective_ranges_merge_and_replay_exhausts() {
    let mut ack = AckTracker::default();
    for seq in [4, 2, 3] {
        assert!(ack.observe(seq).unwrap());
    }
    assert_eq!(ack.snapshot().selective, [2..=4]);
    assert!(ack.observe(1).unwrap());
    assert_eq!(
        ack.snapshot(),
        Ack {
            contiguous: 4,
            selective: vec![]
        }
    );
    assert!(ack.observe(4101).is_err());

    let mut now = Instant::now();
    let mut sender = ReliableSender::new(context());
    let first = send(&mut sender, now, 1);
    assert!(
        sender
            .replay_due(now + Duration::from_millis(99))
            .unwrap()
            .is_empty()
    );
    for attempt in 1..=10 {
        now += Duration::from_secs(2);
        let replay = sender.replay_due(now).unwrap().pop().unwrap();
        assert_eq!(replay.envelope.message_id, first.message_id);
        assert_eq!(replay.degraded, attempt >= 5);
    }
    assert!(
        matches!(sender.replay_due(now + Duration::from_secs(2)), Err(SessionFault::ReplayExhausted(id)) if id == first.message_id)
    );
}

#[test]
fn send_overflow_does_not_evict_or_consume_a_sequence() {
    let now = Instant::now();
    let mut sender = ReliableSender::new(context());
    for seq in 1..=512 {
        assert_eq!(send(&mut sender, now, 1).seq, seq);
    }
    let mut epoch = SessionEpoch::SENTINEL;
    epoch.match_id = context().match_id;
    assert!(matches!(
        sender.send(ProtocolStream::Session, epoch, 0, vec![1], now),
        Err(SessionFault::OverflowDisconnect)
    ));
    sender
        .acknowledge(
            ProtocolStream::Session,
            &Ack {
                contiguous: 1,
                selective: vec![],
            },
        )
        .unwrap();
    assert_eq!(send(&mut sender, now, 1).seq, 513);
}
