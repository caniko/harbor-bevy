use bevy::prelude::*;
use harbor_bevy_core::fatal::FatalCode;
use harbor_bevy_net::*;
use veritasium_wire::{PeerIdBytes, PlayerId};

#[test]
fn namespaces_include_the_configured_scope() {
    let mut config = NetConfig::default();
    let topic = config.match_topic(&[0xab; 16]);
    config.session_id = Some("run-1".into());
    assert_ne!(config.match_topic(&[0xab; 16]), topic);
    assert_ne!(config.control_domain("a"), config.control_domain("b"));
    assert_eq!(config.to_thespan_config().app_id, config.app_id);
}

#[test]
fn stable_fatal_codes_and_buckets() {
    assert_eq!(NetFatal::Desync.exit_code(), 12);
    assert_eq!(
        NetFatal::CommitmentFailure { retries: 5 }.label(),
        "commitment_failure"
    );
    assert_eq!(trace::recovery_latency_bucket(0.1), "0.1");
    assert_eq!(trace::recovery_latency_bucket(30.0), "30");
    assert_eq!(trace::recovery_latency_bucket(30.001), "gt_30");
}

#[test]
fn rng_resumes_the_same_stream_on_both_sides() {
    let (a, b) = ([3; 32], [9; 32]);
    let mut one = SharedRng::from_negotiated_nonces(a, b);
    let mut two = SharedRng::from_negotiated_nonces(b, a);
    assert_eq!(one.next_u64(), two.next_u64());
    let state = one.snapshot_state().unwrap();
    assert_eq!(state.words, 1);
    let mut resumed = SharedRng::restore_from_state(state).unwrap();
    assert_eq!(resumed.next_u64(), two.next_u64());
    assert!(
        SharedRng::restore_from_state(SharedRngState {
            words: (1 << 20) + 1,
            ..state
        })
        .is_err()
    );
}

#[test]
fn discovery_wire_values_are_strict() {
    assert_eq!(GameStatus::try_from(2), Ok(GameStatus::Full));
    assert!(GameStatus::try_from(3).is_err());
    assert_eq!(GameBackend::try_from(3), Ok(GameBackend::WebRtc));
    assert!(GameBackend::try_from(4).is_err());
}

#[test]
fn lifecycle_recovery_leaves_topology_alone() {
    #[derive(Resource, Default)]
    struct Hooks(Vec<&'static str>);
    let mut app = App::new();
    app.add_plugins(ConnectionLifecyclePlugin)
        .init_resource::<Hooks>()
        .add_observer(|_: On<SessionSetupRequested>, mut hooks: ResMut<Hooks>| {
            hooks.0.push("setup")
        })
        .add_observer(|_: On<SessionResumeRequested>, mut hooks: ResMut<Hooks>| {
            hooks.0.push("resume")
        })
        .add_observer(|_: On<SessionCleanupRequested>, mut hooks: ResMut<Hooks>| {
            hooks.0.push("cleanup")
        });
    app.world_mut()
        .resource_mut::<NextState<ConnectionPhase>>()
        .set(ConnectionPhase::Connected);
    app.update();
    app.world_mut()
        .resource_mut::<NextState<ConnectionPhase>>()
        .set(ConnectionPhase::InTournament);
    for health in [
        TransportHealth::Reconnecting,
        TransportHealth::Rejoining,
        TransportHealth::Lost,
        TransportHealth::Healthy,
    ] {
        app.world_mut()
            .resource_mut::<NextState<TransportHealth>>()
            .set(health);
        app.update();
        assert_eq!(
            *app.world().resource::<State<ConnectionPhase>>().get(),
            ConnectionPhase::InTournament
        );
    }
    app.world_mut()
        .resource_mut::<NextState<ConnectionPhase>>()
        .set(ConnectionPhase::Disconnecting);
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<State<ConnectionPhase>>().get(),
        ConnectionPhase::Offline
    );
    assert_eq!(
        *app.world().resource::<State<TransportHealth>>().get(),
        TransportHealth::Inactive
    );
    assert_eq!(
        app.world().resource::<Hooks>().0,
        ["setup", "resume", "cleanup"]
    );
}

#[test]
fn equal_peers_agree_and_cannot_replace_a_commit() {
    let mut left = PeerIdNegotiationState::default();
    let mut right = PeerIdNegotiationState::default();
    let left_commit = left.begin(PeerIdBytes::from([2]), [3; 32]).unwrap();
    let right_commit = right.begin(PeerIdBytes::from([1]), [9; 32]).unwrap();
    left.receive_commit(right_commit).unwrap();
    right.receive_commit(left_commit).unwrap();
    assert_eq!(
        left.receive_commit(left_commit),
        Err(PeerIdCommitError::ConflictingCommit)
    );
    let a = left.reveal().unwrap();
    let b = right.reveal().unwrap();
    assert!(left.receive_reveal(a).is_err());
    let slots = left.receive_reveal(b).unwrap().unwrap();
    let theirs = right.receive_reveal(a).unwrap().unwrap();
    assert_eq!(slots.local, PlayerId::ZERO);
    assert_eq!(theirs.local, PlayerId::ONE);
    assert_eq!(slots.match_id, theirs.match_id);
    assert_eq!(left.receive_reveal(b).unwrap(), None);
    let mut restored = PeerIdNegotiationState::default();
    restored.restore_for_rejoin(slots.clone()).unwrap();
    restored.restore_for_rejoin(slots.clone()).unwrap();
    assert_eq!(restored.assignment(), Some(&slots));
    assert_eq!(
        restored.receive_commit(left_commit),
        Err(PeerIdCommitError::ConflictingCommit)
    );
}
