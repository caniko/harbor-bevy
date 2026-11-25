use bevy::{prelude::*, state::app::StatesPlugin};
use harbor_bevy_net::*;
use libp2p::{PeerId, identity::Keypair};
use std::time::Duration;

#[test]
fn gossip_waits_for_subscription_and_ack_and_keeps_fifo_bounds() {
    let config = NetConfig {
        protocol_prefix: "/example".into(),
        ..Default::default()
    };
    let epoch = SessionEpoch::new([7; 16], BattleNumber(3), RoundNumber(2));
    let topic = config.match_topic(&epoch.match_id);
    let envelope = |seq| GossipEnvelope {
        kind: 12,
        player_id: PlayerId::ZERO,
        seq: OutboundSeq(seq),
        epoch,
        payload: vec![42],
    };
    let mut state = GossipState::default();
    assert_eq!(state.readiness(), &GossipReadiness::Disabled);
    state.subscribe(&config, epoch.match_id, false);
    for seq in 1..=128 {
        assert!(state.outbound(envelope(seq)).unwrap().is_none());
        assert!(state.inbound(envelope(seq)).is_none());
    }
    assert_eq!(
        state.outbound(envelope(129)).unwrap_err(),
        GossipQueueError::Full
    );
    state.inbound(envelope(128));
    assert_eq!(state.early_inbound_dropped_count(), 0);
    state.inbound(envelope(129));
    assert_eq!(state.early_inbound_dropped_count(), 1);
    assert!(!state.acknowledge(epoch.match_id));
    assert!(state.local_subscribed(&topic));
    assert_eq!(
        state.readiness(),
        &GossipReadiness::LocalSubscribed {
            topic: topic.clone()
        }
    );
    assert!(state.acknowledge(epoch.match_id));
    let (outgoing, incoming) = state.drain_ready();
    assert_eq!(outgoing, (1..=128).map(envelope).collect::<Vec<_>>());
    assert_eq!(incoming, (2..=129).map(envelope).collect::<Vec<_>>());
    assert_eq!(state.early_inbound_dropped_count(), 1);
    assert_eq!(state.outbound(envelope(130)).unwrap(), Some(envelope(130)));
    state.subscribe(&config, epoch.match_id, true);
    assert!(state.local_subscribed(&topic));
    assert_eq!(
        state.readiness(),
        &GossipReadiness::AwaitingStateSyncAck { topic }
    );
    assert!(!state.acknowledge(epoch.match_id));
    assert!(state.state_sync_acknowledged());
    assert!(state.acknowledge(epoch.match_id));
    let id = envelope(1).broadcast_id();
    let mut bytes = vec![1, 0, 12, 0];
    bytes.extend([7; 16]);
    bytes.extend(3_u32.to_le_bytes());
    bytes.extend(2_u32.to_le_bytes());
    bytes.push(0);
    bytes.extend(1_u64.to_le_bytes());
    bytes.extend(blake3::hash(&[42]).as_bytes());
    assert_eq!(id, blake3::hash(&bytes).as_bytes()[..20]);
    assert_ne!(id, envelope(2).broadcast_id());
    let action = GossipGameAction {
        action: "action".to_string(),
        effects: vec!["effect".into()],
    };
    let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&action).unwrap();
    assert_eq!(
        rkyv::from_bytes::<GossipGameAction<String>, rkyv::rancor::Error>(&encoded).unwrap(),
        action
    );
}

#[test]
fn liveness_uses_elapsed_proof_time_and_wraps_without_zero() {
    let mut watchdog = LivenessWatchdog::default();
    watchdog.next_nonce = u64::MAX;
    let secs = Duration::from_secs;
    assert_eq!(watchdog.tick(secs(4)).unwrap(), None);
    assert_eq!(
        watchdog.tick(secs(5)).unwrap(),
        Some(LivenessPingEvent { nonce: u64::MAX })
    );
    assert_eq!(
        watchdog.tick(secs(10)).unwrap(),
        Some(LivenessPingEvent { nonce: 1 })
    );
    assert!(!watchdog.acknowledge(99, secs(14)));
    assert!(watchdog.acknowledge(1, secs(14)));
    assert!(watchdog.tick(secs(28)).is_ok());
    assert!(matches!(
        watchdog.tick(secs(29)),
        Err(DisconnectReason::LivenessStalled {
            elapsed_secs: 15.,
            ..
        })
    ));
    assert_eq!(watchdog.status, LivenessStatus::Stalled);
    assert_eq!(watchdog.tick(secs(30)).unwrap(), None);
}

#[test]
fn rejoin_retry_and_snapshot_barrier_have_exact_boundaries() {
    let secs = Duration::from_secs;
    let mut retry = RejoinRetryState::new(RejoinStep::RejoinRequest, secs(0));
    assert!(!retry.poll(secs(0)).unwrap());
    assert!(retry.poll(secs(1)).unwrap());
    assert!(!retry.poll(secs(2)).unwrap());
    assert!(retry.poll(secs(3)).unwrap());
    assert!(!retry.poll(secs(6)).unwrap());
    assert_eq!(
        retry.poll(secs(7)),
        Err(RejoinStepTimeout {
            step: RejoinStep::RejoinRequest,
            attempts: 3
        })
    );
    let mut barrier = StateSyncSnapshotApplyBarrier::default();
    barrier.start();
    barrier.tick();
    assert!(barrier.is_active());
    barrier.tick();
    barrier.tick();
    assert!(!barrier.is_active());
    let recovery = ReconnectionState::new(secs(15), &NetConfig::default());
    assert!(!recovery.expired(secs(74)));
    assert!(recovery.expired(secs(75)));
    let mut mismatch = StateMismatchRecovery::default();
    assert!(
        mismatch
            .mismatch((BattleNumber(1), RoundNumber(0), TurnNumber(0)))
            .is_ok()
    );
    assert_eq!(
        mismatch.mismatch((BattleNumber(1), RoundNumber(0), TurnNumber(1))),
        Err(SessionFault::Desync)
    );
}

#[test]
fn mesh_allocates_slots_and_pins_signatures_to_expected_identity() {
    let key = Keypair::generate_ed25519();
    let peer = key.public().to_peer_id();
    let other_key = Keypair::generate_ed25519();
    let other = other_key.public().to_peer_id();
    let mut mesh = SlotPeerMap::new(1, 0);
    assert_eq!(mesh.capacity(), 8);
    assert_eq!(mesh.allocate(peer), Some(0));
    assert_eq!(mesh.allocate(other), Some(2));
    assert_eq!(mesh.slot_for(&peer), Some(0));
    let mut tight_mesh = SlotPeerMap::new(1, 2);
    assert!(!tight_mesh.bind(1, peer));
    assert!(tight_mesh.bind(0, peer));
    assert!(!tight_mesh.bind(0, other));
    assert_eq!(tight_mesh.allocate(other), None);
    assert!(matches!(mesh.session_for(0), Err(SlotSendError::MissingSession(id)) if id == peer));
    assert!(matches!(
        mesh.session_for(7),
        Err(SlotSendError::UnknownSlot(7))
    ));
    let addr = "/ip4/127.0.0.1/udp/40000/quic-v1".parse().unwrap();
    assert!(mesh.dial_addr(&peer, &addr).is_none());
    mesh.listen_ports.insert(peer, 30000);
    assert_eq!(
        mesh.dial_addr(&peer, &addr).unwrap().to_string(),
        "/ip4/127.0.0.1/udp/30000/quic-v1"
    );
    let config = NetConfig::default();
    let mut signed =
        SignedControlEnvelope::sign(vec![1, 2], &key, &config, "mesh", |bytes| bytes.clone())
            .unwrap();
    assert!(signed.verify(&peer, &config, "mesh", |bytes| bytes.clone()));
    assert!(!signed.verify(&other, &config, "mesh", |bytes| bytes.clone()));
    let forged =
        SignedControlEnvelope::sign(vec![1, 2], &other_key, &config, "mesh", Vec::clone).unwrap();
    assert!(forged.verify(&other, &config, "mesh", Vec::clone));
    assert!(!forged.verify(&peer, &config, "mesh", Vec::clone));
    assert!(!signed.verify(&peer, &config, "different", |bytes| bytes.clone()));
    signed.payload[0] = 9;
    assert!(!signed.verify(&peer, &config, "mesh", |bytes| bytes.clone()));
    let blocked = BlockedPeerBytes {
        ids: vec![peer.to_bytes()],
        platform_ids: vec![12],
    };
    assert!(blocked.contains_peer(&peer));
}

#[test]
fn admission_prefers_transport_identity_and_never_admits_on_observers() {
    #[derive(Default, Resource)]
    struct Accepted(Vec<SendControl<ObserverAccepted>>);
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, StatesPlugin, AdmissionPlugin))
        .insert_resource(ObserverAdmission {
            match_id: [7; 16],
            genesis_hash: [8; 32],
        })
        .init_resource::<Accepted>()
        .add_observer(
            |event: On<SendControl<ObserverAccepted>>, mut accepted: ResMut<Accepted>| {
                accepted.0.push(event.event().clone());
            },
        );
    let claimed = PeerId::random();
    let authenticated = PeerId::random();
    let hello = ObserverHello {
        peer_id_bytes: claimed.to_bytes().into(),
        witness_id: [9; 16],
        public_key: [10; 32],
        name: "observer".into(),
    };
    app.world_mut().trigger(FromNetwork {
        inner: hello.clone(),
        from: Some(PeerIdBytes::default()),
    });
    assert!(app.world().resource::<ObserverPeers>().0.is_empty());
    app.world_mut().trigger(FromNetwork {
        inner: hello.clone(),
        from: Some(authenticated.to_bytes().into()),
    });
    app.world_mut().flush();
    assert_eq!(
        app.world()
            .resource::<ObserverPeers>()
            .0
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [authenticated]
    );
    assert_eq!(
        app.world().resource::<Accepted>().0[0].message.genesis_hash,
        [8; 32]
    );
    app.insert_resource(PeerRole::Observer);
    app.world_mut().trigger(FromNetwork {
        inner: hello,
        from: None,
    });
    assert_eq!(app.world().resource::<ObserverPeers>().0.len(), 1);
}
