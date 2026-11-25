use bevy::prelude::*;
use bevy_replicated::{ChainHash, dedup::KindIndex, epoch::SessionEpoch};
use harbor_bevy_core::observer_ordering::AppObserverOrderingExt as _;
use harbor_bevy_net::*;
use veritasium_wire::{OutboundSeq, PeerIdBytes};

struct Domain;
struct Router;
#[derive(Clone, Copy)]
struct Kind;
#[derive(Clone, Event)]
struct Tick(u8);

impl GameNet for Domain {
    type Router = Router;
    type Packet = Tick;
    type Snapshot = Tick;
    type Hidden = Tick;
}

impl KindIndex for Kind {
    const COUNT: usize = 1;
    fn kind_index(self) -> usize {
        0
    }
}

impl SessionRouter for Router {
    fn accepts_peer(&self, _: &PeerIdBytes) -> bool {
        true
    }
    fn route(
        &mut self,
        from: PeerIdBytes,
        input: &[u8],
    ) -> Result<Option<PendingEvent>, SessionFault> {
        let tick = <Tick as Packet>::decode(input)
            .map_err(|e| SessionFault::ValidationViolation(e.to_string()))?;
        Ok(Some(PendingEvent::new(
            IngressLane::Reliable,
            Some(from),
            tick,
        )))
    }
}

impl Packet for Tick {
    type Kind = Kind;
    type Error = std::io::Error;
    fn kind(&self) -> Kind {
        Kind
    }
    fn sequence(&self) -> OutboundSeq {
        OutboundSeq(u64::from(self.0))
    }
    fn epoch(&self) -> SessionEpoch {
        SessionEpoch::SENTINEL
    }
    fn encode(&self) -> Result<Vec<u8>, Self::Error> {
        Ok(vec![self.0])
    }
    fn decode(bytes: &[u8]) -> Result<Self, Self::Error> {
        match bytes {
            [tick] => Ok(Self(*tick)),
            _ => Err(std::io::ErrorKind::InvalidData.into()),
        }
    }
}

impl SnapshotCodec for Tick {
    type Error = std::io::Error;
    fn capture(world: &World) -> Result<Self, Self::Error> {
        Ok(Self(world.resource::<Counter>().0))
    }
    fn apply(self, world: &mut World) -> Result<(), Self::Error> {
        world.resource_mut::<Counter>().0 = self.0;
        Ok(())
    }
    fn chain_hash(&self) -> ChainHash {
        *blake3::hash(&[self.0]).as_bytes()
    }
    fn progress(&self) -> SnapshotProgress {
        (
            BattleNumber(1),
            RoundNumber(0),
            TurnNumber(u32::from(self.0)),
        )
    }
    fn encode(&self) -> Result<Vec<u8>, Self::Error> {
        Packet::encode(self)
    }
    fn decode(bytes: &[u8]) -> Result<Self, Self::Error> {
        <Self as Packet>::decode(bytes)
    }
}

impl CommitPayload for Tick {
    type Error = std::io::Error;
    fn to_commit_bytes(&self) -> Result<Vec<u8>, Self::Error> {
        Packet::encode(self)
    }
    fn from_commit_bytes(bytes: &[u8]) -> Result<Self, Self::Error> {
        <Self as Packet>::decode(bytes)
    }
}

#[derive(Default, Resource)]
struct Counter(u8);

fn app(config: NetConfig) -> App {
    let mut app = App::new();
    app.insert_resource(config)
        .init_resource::<Counter>()
        .add_plugins((
            SessionPlugin::<Domain>::default(),
            ReliablePlugin::<Domain>::default(),
        ))
        .add_network_bridge_observer(|event: On<FromNetwork<Tick>>, mut commands: Commands| {
            commands.trigger(event.inner.clone());
        })
        .add_gameplay_mutation_observer(|tick: On<Tick>, mut count: ResMut<Counter>| {
            count.0 += tick.0;
        })
        .add_validation_observer(|_: On<Tick>, mut count: ResMut<Counter>| {
            count.0 *= 2;
        });
    app
}

#[test]
fn typed_loopback_obeys_observer_order_and_frame_cap() {
    let mut app = app(NetConfig {
        max_events_per_frame: 1,
        ..Default::default()
    });
    let ingress = app.world().resource::<SessionIngressHandle>().clone();
    ingress
        .inject(IngressLane::Reliable, None, Tick(1))
        .unwrap();
    ingress
        .inject(IngressLane::Reliable, None, Tick(2))
        .unwrap();
    app.update();
    assert_eq!(app.world().resource::<Counter>().0, 1);
    app.update();
    assert_eq!(app.world().resource::<Counter>().0, 4);
    let snap = <Domain as GameNet>::Snapshot::capture(app.world()).unwrap();
    assert_eq!(snap.sequence(), OutboundSeq(4));
    let hidden =
        <Domain as GameNet>::Hidden::from_commit_bytes(&snap.to_commit_bytes().unwrap()).unwrap();
    hidden.apply(app.world_mut()).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 4);
}

#[test]
fn telemetry_drops_oldest_and_reliable_overflow_requests_disconnect() {
    let mut app = app(NetConfig {
        ingress_capacity: 1,
        ..Default::default()
    });
    let ingress = app.world().resource::<SessionIngressHandle>().clone();
    ingress
        .inject(IngressLane::Telemetry, None, Tick(2))
        .unwrap();
    assert_eq!(ingress.telemetry_dropped_count(), 0);
    ingress
        .inject(IngressLane::Telemetry, None, Tick(3))
        .unwrap();
    assert_eq!(ingress.telemetry_dropped_count(), 1);
    app.update();
    assert_eq!(app.world().resource::<Counter>().0, 3);

    app.world_mut()
        .resource_mut::<NextState<ConnectionPhase>>()
        .set(ConnectionPhase::Connected);
    app.update();
    ingress
        .inject(IngressLane::Reliable, None, Tick(1))
        .unwrap();
    assert_eq!(
        ingress.inject(IngressLane::Reliable, None, Tick(1)),
        Err(SessionFault::OverflowDisconnect)
    );
    app.update();
    assert_eq!(app.world().resource::<Counter>().0, 3);
    assert_eq!(
        *app.world().resource::<State<ConnectionPhase>>().get(),
        ConnectionPhase::Disconnecting
    );
    app.update();
    assert_eq!(
        *app.world().resource::<State<ConnectionPhase>>().get(),
        ConnectionPhase::Offline
    );
    assert!(ingress.is_shutting_down());
    assert_eq!(ingress.telemetry_dropped_count(), 1);
}

#[test]
fn deferred_reliable_events_keep_fifo_order() {
    #[derive(Resource)]
    struct Ready;

    let mut app = app(NetConfig::default());
    let ingress = app.world().resource::<SessionIngressHandle>().clone();
    ingress
        .push(
            PendingEvent::new(IngressLane::Reliable, None, Tick(1))
                .defer_until(World::contains_resource::<Ready>),
        )
        .unwrap();
    ingress
        .inject(IngressLane::Reliable, None, Tick(2))
        .unwrap();
    ingress
        .inject(IngressLane::Telemetry, None, Tick(3))
        .unwrap();
    app.update();
    assert_eq!(app.world().resource::<Counter>().0, 3);
    app.world_mut().insert_resource(Ready);
    app.update();
    assert_eq!(app.world().resource::<Counter>().0, 16);
}

#[test]
fn deferred_overflow_reports_failure_in_the_same_frame() {
    #[derive(Resource, Default)]
    struct Faults(Vec<SessionFault>);
    let mut app = app(NetConfig {
        max_deferred_events: 1,
        max_events_per_frame: 2,
        ..Default::default()
    });
    app.init_resource::<Faults>().add_observer(
        |fault: On<SessionFault>, mut faults: ResMut<Faults>| {
            faults.0.push(fault.event().clone());
        },
    );
    let ingress = app.world().resource::<SessionIngressHandle>();
    ingress
        .push(PendingEvent::new(IngressLane::Reliable, None, Tick(1)).defer_until(|_| false))
        .unwrap();
    ingress
        .inject(IngressLane::Reliable, None, Tick(2))
        .unwrap();
    app.update();
    assert_eq!(
        app.world().resource::<Faults>().0,
        [SessionFault::OverflowDisconnect]
    );
    assert_eq!(app.world().resource::<Counter>().0, 0);
}

#[test]
fn transport_shutdown_closes_ingress_and_unregisters_the_well_known_actor() {
    let key = thespan::identity::Keypair::generate_ed25519();
    let id =
        piying::actor::ActorId::new_with_peer_id(SESSION_ACTOR_SEQUENCE, key.public().to_peer_id());
    let config = NetConfig::default();
    let ingress = SessionIngressHandle::new(1);
    let (mut transport, teardown) = spawn_session(
        &config,
        thespan::TransportType::Lan(thespan::LanConfig::default()),
        Some(key.clone()),
        Router,
        ingress.clone(),
    )
    .unwrap();
    assert!(piying::remote::is_registered_locally(id));
    ingress
        .inject(IngressLane::Reliable, None, Tick(1))
        .unwrap();
    transport.shutdown_now();
    transport.shutdown_now();
    assert!(ingress.is_shutting_down());
    assert!(!piying::remote::is_registered_locally(id));
    assert!(teardown.is_stopped());
    // The same persistent identity can be used immediately without stale registration.
    let (mut second, _) = spawn_session(
        &config,
        thespan::TransportType::Lan(thespan::LanConfig::default()),
        Some(key),
        Router,
        SessionIngressHandle::new(1),
    )
    .unwrap();
    second.shutdown_now();
}

#[test]
fn bootstrap_failure_stops_registered_actor_before_runtime_drop_and_allows_reopen() {
    let key = thespan::identity::Keypair::generate_ed25519();
    let ingress = SessionIngressHandle::new(1);
    let (dropped, drop_result) = std::sync::mpsc::channel();
    let failed = spawn_session(
        &NetConfig {
            protocol_prefix: "missing-leading-slash".into(),
            ..Default::default()
        },
        thespan::TransportType::Lan(thespan::LanConfig::default()),
        Some(key.clone()),
        DropProbe(ingress.clone(), dropped),
        ingress.clone(),
    );
    assert!(matches!(failed, Err(SessionStartError::Transport(_))));
    assert!(ingress.is_shutting_down());
    assert_eq!(drop_result.try_recv(), Ok(true));
    assert!(
        spawn_session(
            &NetConfig::default(),
            thespan::TransportType::Lan(thespan::LanConfig::default()),
            Some(key),
            Router,
            SessionIngressHandle::new(1),
        )
        .is_ok()
    );
}

struct DropProbe(SessionIngressHandle, std::sync::mpsc::Sender<bool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        // Tokio also enters its context when dropping unfinished tasks. Check
        // that session cleanup already closed ingress before dropping the actor.
        let stopped_on_transport = self.0.is_shutting_down()
            && tokio::runtime::Handle::try_current().is_ok()
            && std::thread::current().name() == Some("thespan-lan");
        let _ = self.1.send(stopped_on_transport);
    }
}

impl SessionRouter for DropProbe {
    fn accepts_peer(&self, _: &PeerIdBytes) -> bool {
        false
    }

    fn route(&mut self, _: PeerIdBytes, _: &[u8]) -> Result<Option<PendingEvent>, SessionFault> {
        unreachable!()
    }
}

#[test]
fn actor_overflow_stops_session_without_consumer_shutdown() {
    use std::time::{Duration, Instant};
    use thespan::{ActorErrorKind, ActorEvent, LanConfig, NetworkCommand, TransportType};

    let key = thespan::identity::Keypair::generate_ed25519();
    let ingress = SessionIngressHandle::new(1);
    let (dropped, drop_result) = std::sync::mpsc::channel();
    let (transport, teardown) = spawn_session(
        &NetConfig::default(),
        TransportType::Lan(LanConfig {
            gossipsub_enabled: false,
            ..Default::default()
        }),
        Some(key.clone()),
        DropProbe(ingress.clone(), dropped),
        ingress.clone(),
    )
    .unwrap();
    for _ in 0..=thespan::spawn::THESPAN_EVENT_CHANNEL_CAPACITY {
        // One disabled-GossipSub error per accepted command; leave events undrained.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match transport.send(NetworkCommand::Subscribe {
                topic: "test".into(),
            }) {
                Ok(()) => break,
                Err(error) if error.to_string() == "mailbox full" => {
                    assert!(Instant::now() < deadline, "command mailbox stayed full");
                    std::thread::yield_now();
                }
                Err(_) if teardown.is_stopped() || ingress.is_shutting_down() => break,
                Err(error) => panic!("transport stopped before overflow: {error}"),
            }
        }
    }
    assert_eq!(drop_result.recv_timeout(Duration::from_secs(5)), Ok(true));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !teardown.is_stopped() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(ingress.is_shutting_down());
    assert!(
        std::iter::from_fn(|| transport.event_rx.try_recv().ok()).any(|event| matches!(
            event,
            ActorEvent::Error {
                kind: ActorErrorKind::ReliableEventOverflow,
                ..
            }
        ))
    );
    assert!(
        spawn_session(
            &NetConfig::default(),
            TransportType::Lan(LanConfig::default()),
            Some(key),
            Router,
            SessionIngressHandle::new(1),
        )
        .is_ok()
    );
}

#[test]
fn signed_session_frame_reaches_the_admitted_remote_router() {
    use libp2p::multiaddr::Protocol;
    use std::{
        net::Ipv4Addr,
        sync::{Mutex, mpsc},
        time::{Duration, Instant},
    };
    use thespan::{ActorEvent, LanConfig, TransportType, identity::Keypair};

    struct Admitted {
        peer: PeerIdBytes,
        routed: mpsc::Sender<u8>,
        resume: Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl SessionRouter for Admitted {
        fn accepts_peer(&self, peer: &PeerIdBytes) -> bool {
            *peer == self.peer
        }
        fn route(
            &mut self,
            _: PeerIdBytes,
            bytes: &[u8],
        ) -> Result<Option<PendingEvent>, SessionFault> {
            self.routed.send(bytes[0]).unwrap();
            if let Some(resume) = self.resume.get_mut().unwrap().take() {
                let _ = resume.recv_timeout(Duration::from_secs(5));
            }
            Ok(None)
        }
    }

    fn wait_for<T>(
        transport: &thespan_bevy::NetworkResource,
        select: impl Fn(ActorEvent) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for transport event"
            );
            if let Some(value) = select(transport.event_rx.recv_timeout(remaining).unwrap()) {
                return value;
            }
        }
    }

    let signer = Keypair::generate_ed25519();
    let (routed, received) = mpsc::channel();
    let (resume, blocked) = mpsc::channel();
    let config = NetConfig::default();
    let mut listener = spawn_session(
        &config,
        TransportType::Lan(LanConfig::default()),
        Some(Keypair::generate_ed25519()),
        Admitted {
            peer: signer.public().to_peer_id().to_bytes().into(),
            routed,
            resume: Mutex::new(Some(blocked)),
        },
        SessionIngressHandle::new(2),
    )
    .unwrap()
    .0;
    let addr = wait_for(&listener, |event| match event {
        ActorEvent::ListenStarted { addr } => {
            addr.replace(0, |_| Some(Protocol::Ip4(Ipv4Addr::LOCALHOST)))
        }
        _ => None,
    });
    let mut dialer = spawn_session(
        &config,
        TransportType::Lan(LanConfig {
            dial_addr: Some(addr),
            ..Default::default()
        }),
        Some(signer.clone()),
        Router,
        SessionIngressHandle::new(2),
    )
    .unwrap()
    .0;
    wait_for(&dialer, |event| match event {
        ActorEvent::PeerConnected { peer_id, .. } if peer_id == listener.local_peer_id => Some(()),
        _ => None,
    });
    let mut mesh = SlotPeerMap::new(0, 2);
    let slot = mesh.allocate(listener.local_peer_id).unwrap();
    mesh.sessions.insert(
        listener.local_peer_id,
        RemoteSessionRef::for_peer_with_sender(listener.local_peer_id, dialer.command.clone()),
    );
    // The enqueue API has no delivery guarantee. Exercise the same bounded
    // retry pattern consumers use around the first send on a new connection.
    let deadline = Instant::now() + Duration::from_secs(5);
    let routed = loop {
        mesh.send(slot, &config, &signer, vec![42]).unwrap();
        match received.recv_timeout(Duration::from_millis(250)) {
            Ok(value) => break value,
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
            Err(error) => panic!("signed session frame was not routed: {error}"),
        }
    };
    assert_eq!(routed, 42);

    // The listener cannot process or acknowledge these tells until we release
    // its router. The sender actor can still hand them to Piying.
    let accepted = (0..=128)
        .find(|_| match mesh.send(slot, &config, &signer, vec![43]) {
            Ok(()) => false,
            Err(SlotSendError::Full) => true,
            Err(error) => panic!("unexpected send error: {error}"),
        })
        .expect("a stalled receiver must exhaust the reliable window");
    assert!((1..=64).contains(&accepted));
    resume.send(()).unwrap();
    for _ in 0..accepted {
        while received.recv_timeout(Duration::from_secs(5)).unwrap() != 43 {}
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match mesh.send(slot, &config, &signer, vec![44]) {
            Ok(()) => break,
            Err(SlotSendError::Full) => {
                assert!(
                    Instant::now() < deadline,
                    "acknowledgements did not release permits"
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("unexpected send error: {error}"),
        }
    }
    assert_eq!(received.recv_timeout(Duration::from_secs(5)), Ok(44));
    dialer.shutdown_now();
    assert!(matches!(
        mesh.session_for(slot)
            .unwrap()
            .send(&config, &signer, vec![42]),
        Err(SessionSendError::Closed)
    ));
    assert_eq!(
        mesh.send(slot, &config, &signer, vec![42]),
        Err(SlotSendError::Closed)
    );
    listener.shutdown_now();
}

#[test]
fn transport_surfaces_remote_missing_actor_as_a_bevy_event() {
    use libp2p::multiaddr::Protocol;
    use std::{
        net::Ipv4Addr,
        time::{Duration, Instant},
    };
    use thespan::{ActorEvent, LanConfig, TransportType, identity::Keypair};

    let lan = || TransportType::Lan(LanConfig::default());
    // This peer runs the transport but does not register a session actor.
    let listener =
        thespan_bevy::spawn_network_actor_with_keypair(lan(), Keypair::generate_ed25519(), None)
            .unwrap();
    let ActorEvent::ListenStarted { addr } = listener
        .event_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
    else {
        panic!("expected a LAN listen address");
    };
    let signer = Keypair::generate_ed25519();
    let dialer = thespan_bevy::spawn_network_actor_with_keypair(
        TransportType::Lan(LanConfig {
            dial_addr: addr.replace(0, |_| Some(Protocol::Ip4(Ipv4Addr::LOCALHOST))),
            ..Default::default()
        }),
        signer.clone(),
        None,
    )
    .unwrap();
    let peer = listener.local_peer_id;
    let session = RemoteSessionRef::<Router>::for_peer_with_sender(peer, dialer.command.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let mut app = App::new();
    app.add_plugins(ThespanBevyPlugin)
        .insert_resource(dialer)
        .add_observer(move |event: On<RemoteSendFailedEvent>| {
            tx.send(event.event().clone()).unwrap();
        });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app
        .world()
        .resource::<thespan_bevy::NetworkResource>()
        .is_connected(&peer)
    {
        assert!(Instant::now() < deadline, "LAN peers did not connect");
        app.update();
        std::thread::yield_now();
    }
    session
        .send(&NetConfig::default(), &signer, vec![1])
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let failure = loop {
        app.update();
        if let Ok(event) = rx.try_recv() {
            break event;
        }
        assert!(
            Instant::now() < deadline,
            "remote rejection was not delivered"
        );
        std::thread::yield_now();
    };
    assert_eq!(failure.peer_id, peer);
    assert_eq!(failure.error, "actor not running");
}

#[test]
fn two_peer_plugin_group_orders_ingress_and_disconnects_both_axes() {
    use bevy::state::app::StatesPlugin;
    use harbor_bevy_net::{
        ConnectionLostEvent, DisconnectReason, HarborNetPlugins, TransportHealth,
    };
    use thespan::identity::Keypair;
    use veritasium_wire::PlayerId;

    #[derive(Default, Resource)]
    struct Log {
        order: Vec<(&'static str, u8)>,
        commits: Vec<PeerIdCommit>,
        reveals: Vec<PeerIdReveal>,
        lost: Vec<DisconnectReason>,
    }

    fn peer() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            HarborNetPlugins::<Domain>::new(NetConfig {
                max_events_per_frame: 42,
                ..Default::default()
            }),
        ))
        .init_resource::<Log>()
        .init_resource::<Counter>()
        .add_network_bridge_observer(|e: On<FromNetwork<Tick>>, mut log: ResMut<Log>| {
            log.order.push(("bridge", e.inner.0));
        })
        .add_gameplay_mutation_observer(|e: On<FromNetwork<Tick>>, mut log: ResMut<Log>| {
            log.order.push(("mutation", e.inner.0));
        })
        .add_validation_observer(|e: On<FromNetwork<Tick>>, mut log: ResMut<Log>| {
            log.order.push(("validation", e.inner.0));
        })
        .add_observer(|e: On<PeerCommitReady>, mut log: ResMut<Log>| log.commits.push(e.1))
        .add_observer(|e: On<PeerRevealReady>, mut log: ResMut<Log>| log.reveals.push(e.1))
        .add_observer(|e: On<ConnectionLostEvent>, mut log: ResMut<Log>| {
            log.lost.push(e.0.clone())
        });
        app.world_mut()
            .resource_mut::<NextState<ConnectionPhase>>()
            .set(ConnectionPhase::Negotiating);
        app.update();
        app
    }

    fn deliver<T: Event + Clone>(app: &mut App, from: &PeerIdBytes, value: T) {
        app.world()
            .resource::<SessionIngressHandle>()
            .inject(IngressLane::Reliable, Some(from.clone()), value)
            .unwrap();
        app.update();
    }

    let mut a = peer();
    let mut b = peer();
    let a_id = PeerIdBytes::from(Keypair::generate_ed25519().public().to_peer_id().to_bytes());
    let b_id = PeerIdBytes::from(Keypair::generate_ed25519().public().to_peer_id().to_bytes());
    a.world_mut().trigger(BeginPeerNegotiation(b_id.clone()));
    b.world_mut().trigger(BeginPeerNegotiation(a_id.clone()));
    a.world_mut().flush();
    b.world_mut().flush();
    let a_commit = a.world().resource::<Log>().commits[0];
    let b_commit = b.world().resource::<Log>().commits[0];
    deliver(&mut a, &b_id, b_commit);
    deliver(&mut b, &a_id, a_commit);
    let a_reveal = a.world().resource::<Log>().reveals[0];
    let b_reveal = b.world().resource::<Log>().reveals[0];
    deliver(&mut a, &b_id, b_reveal);
    deliver(&mut b, &a_id, a_reveal);
    let a_seat = a
        .world()
        .resource::<PeerIdNegotiationState>()
        .assignment()
        .unwrap();
    let b_seat = b
        .world()
        .resource::<PeerIdNegotiationState>()
        .assignment()
        .unwrap();
    assert_eq!(a_seat.match_id, b_seat.match_id);
    let mut players = [a_seat.local, b_seat.local];
    players.sort();
    assert_eq!(players, [PlayerId::ZERO, PlayerId::ONE]);

    deliver(&mut a, &b_id, Tick(2));
    deliver(&mut b, &a_id, Tick(3));
    for (app, value) in [(&mut a, 2), (&mut b, 3)] {
        assert_eq!(app.world().resource::<NetConfig>().max_events_per_frame, 42);
        assert_eq!(
            app.world().resource::<Log>().order,
            [
                ("validation", value),
                ("mutation", value),
                ("bridge", value),
            ]
        );
        let ingress = app.world().resource::<SessionIngressHandle>().clone();
        app.world_mut().trigger(SessionFault::ValidationViolation(
            "external validator".into(),
        ));
        app.world_mut().trigger(SessionFault::Desync);
        app.update();
        assert_eq!(
            *app.world().resource::<State<ConnectionPhase>>().get(),
            ConnectionPhase::Disconnecting
        );
        assert_eq!(
            *app.world().resource::<State<TransportHealth>>().get(),
            TransportHealth::Disconnecting
        );
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
            app.world().resource::<Log>().lost,
            [DisconnectReason::InvariantViolation(
                "external validator".into()
            )]
        );
        assert_eq!(app.world().resource::<Counter>().0, 0);
        assert!(ingress.is_shutting_down());
    }
}

#[test]
fn commitment_verifies_and_resends_when_verified_but_unacked() {
    use harbor_bevy_net::{CommitmentMessage as Msg, CommitmentPhase, CommitmentState};
    use std::time::Duration;

    let mut a = CommitmentState::<Tick>::default();
    let mut b = CommitmentState::<Tick>::default();
    let zero = Duration::ZERO;
    let sec = Duration::from_secs;
    a.reset_turn(7);
    b.reset_turn(7);
    let ca = a.begin(&Tick(1), [1; 32], "domain", zero).unwrap();
    let cb = b.begin(&Tick(2), [2; 32], "domain", zero).unwrap();
    let ack_b = a.receive(&cb, "domain", zero).unwrap().reply.unwrap();
    let ack_a = b.receive(&ca, "domain", zero).unwrap().reply.unwrap();
    b.receive(&ack_b, "domain", zero).unwrap();
    let rb = b.take_reveal(zero).unwrap();
    let verified = a.receive(&rb, "domain", zero).unwrap().verified.unwrap();
    assert_eq!(verified.0, 2);
    assert_eq!(a.phase(), CommitmentPhase::Verified);
    assert!(!a.peer_acked());
    assert_eq!(a.tick(sec(1)).unwrap(), Some(ca));
    a.receive(&ack_a, "domain", sec(1)).unwrap();
    let ra = a.take_reveal(sec(1)).unwrap();
    assert_eq!(
        b.receive(&ra, "domain", sec(1))
            .unwrap()
            .verified
            .unwrap()
            .0,
        1
    );
    assert!(a.tick(sec(2)).unwrap().is_none());
    assert!(b.receive(&ra, "domain", sec(1)).unwrap().verified.is_none());

    b.reset_turn(8);
    a.reset_turn(8);
    let ca = a.begin(&Tick(1), [1; 32], "domain", zero).unwrap();
    b.begin(&Tick(2), [2; 32], "domain", zero).unwrap();
    b.receive(&ca, "domain", zero).unwrap();
    assert_eq!(
        b.receive(
            &Msg::Reveal {
                turn: 8,
                payload: vec![9],
                nonce: [1; 32]
            },
            "domain",
            zero
        )
        .err(),
        Some(SessionFault::CommitmentMismatch)
    );
    b.reset_turn(9);
    assert_eq!(b.consecutive_failures(), 1);
    b.begin(&Tick(2), [2; 32], "domain", zero).unwrap();
    for attempt in 1..=3 {
        assert!(b.tick(sec(attempt)).unwrap().is_some());
    }
    assert_eq!(
        b.tick(sec(4)),
        Err(SessionFault::CommitmentRetriesExhausted)
    );
    assert_eq!(b.consecutive_failures(), 2);
}

#[test]
fn snapshot_sync_rejects_oversize_bad_hash_and_rewind_before_applying() {
    let mut source = World::new();
    source.insert_resource(Counter(8));
    let mut destination = World::new();
    destination.insert_resource(Counter(4));
    let epoch = SessionEpoch::new([3; 16], BattleNumber(1), RoundNumber(0));
    let mut wire = GameStateSync::capture::<Tick>(&source, 1, epoch).unwrap();
    let mut sequence = StateSyncTracker::default();
    let mut bad = wire.clone();
    bad.chain_hash[0] ^= 1;
    assert!(matches!(
        sequence.apply::<Tick>(&bad, epoch.match_id, None, &mut destination),
        Err(StateSyncError::HashMismatch)
    ));
    bad = wire.clone();
    bad.snapshot_bytes.resize(STATE_SYNC_ENCODED_LIMIT, 0);
    assert!(matches!(bad.encode(), Err(StateSyncError::Oversized)));
    assert!(matches!(
        GameStateSync::decode(&vec![0; STATE_SYNC_ENCODED_LIMIT + 1]),
        Err(StateSyncError::Oversized)
    ));
    assert!(matches!(
        sequence.apply::<Tick>(&bad, epoch.match_id, None, &mut destination),
        Err(StateSyncError::Oversized)
    ));
    assert_eq!(destination.resource::<Counter>().0, 4);
    wire = GameStateSync::decode(&wire.encode().unwrap()).unwrap();
    sequence
        .apply::<Tick>(&wire, epoch.match_id, None, &mut destination)
        .unwrap();
    assert_eq!(destination.resource::<Counter>().0, 8);
    assert_eq!(sequence.last_applied_sync_sequence(), 1);
    assert!(matches!(
        sequence.apply::<Tick>(&wire, epoch.match_id, None, &mut destination),
        Err(StateSyncError::Stale)
    ));
    source.resource_mut::<Counter>().0 = 7;
    let rewind = GameStateSync::capture::<Tick>(&source, 2, epoch).unwrap();
    assert!(matches!(
        sequence.apply::<Tick>(&rewind, epoch.match_id, None, &mut destination),
        Err(StateSyncError::Rewind)
    ));
    assert_eq!(destination.resource::<Counter>().0, 8);
}

fn connected_pair(left: u8, right: u8) -> ((App, PeerIdBytes), (App, PeerIdBytes)) {
    use bevy::state::app::StatesPlugin;
    use thespan::identity::Keypair;

    let a_id = PeerIdBytes::from(Keypair::generate_ed25519().public().to_peer_id().to_bytes());
    let b_id = PeerIdBytes::from(Keypair::generate_ed25519().public().to_peer_id().to_bytes());
    let mut a = PeerIdNegotiationState::default();
    let mut b = PeerIdNegotiationState::default();
    let a_commit = a.begin(b_id.clone(), [1; 32]).unwrap();
    let b_commit = b.begin(a_id.clone(), [2; 32]).unwrap();
    a.receive_commit(b_commit).unwrap();
    b.receive_commit(a_commit).unwrap();
    let a_reveal = a.reveal().unwrap();
    let b_reveal = b.reveal().unwrap();
    let a_seat = a.receive_reveal(b_reveal).unwrap().unwrap();
    let b_seat = b.receive_reveal(a_reveal).unwrap().unwrap();

    fn init(counter: u8, negotiation: PeerIdNegotiationState, seats: PeerSlotsAssigned) -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            HarborNetPlugins::<Domain>::new(NetConfig::default()),
        ))
        .insert_resource(Counter(counter))
        .insert_resource(negotiation);
        app.world_mut().trigger(seats);
        app.world_mut()
            .resource_mut::<NextState<ConnectionPhase>>()
            .set(ConnectionPhase::Connected);
        app.update();
        app.update();
        app
    }
    (
        (init(left, a, a_seat), a_id),
        (init(right, b, b_seat), b_id),
    )
}

#[derive(Resource)]
struct ControlOut<T: Event + Clone>(Vec<SendControl<T>>);

fn record_control<T: Event + Clone>(app: &mut App) {
    app.insert_resource(ControlOut::<T>(vec![])).add_observer(
        |event: On<SendControl<T>>, mut out: ResMut<ControlOut<T>>| {
            out.0.push(event.event().clone())
        },
    );
}

fn take_control<T: Event + Clone>(app: &mut App, to: &PeerIdBytes) -> T {
    app.world_mut().flush();
    let mut out = app.world_mut().resource_mut::<ControlOut<T>>();
    assert!(!out.0.is_empty(), "expected {}", std::any::type_name::<T>());
    let sent = out.0.remove(0);
    assert_eq!(&sent.to, to);
    sent.message
}

fn arrived<T: Event + Clone>(app: &mut App, from: &PeerIdBytes, value: T) {
    app.world_mut().trigger(FromNetwork {
        inner: value,
        from: Some(from.clone()),
    });
    app.world_mut().flush();
}

#[test]
fn rejoin_routes_serialized_snapshot_and_replays_ack_without_reapplying() {
    let ((mut a, a_id), (mut b, b_id)) = connected_pair(4, 8);
    record_control::<RejoinRequest>(&mut a);
    record_control::<RequestStateSync>(&mut a);
    record_control::<StateSyncAck>(&mut a);
    record_control::<RejoinAccepted>(&mut b);
    record_control::<RejoinDenied>(&mut b);
    record_control::<GameStateSync>(&mut b);
    b.world_mut()
        .resource_mut::<NextState<TransportHealth>>()
        .set(TransportHealth::Reconnecting);
    b.update();
    a.world_mut().trigger(BeginRejoin);
    a.update();
    let req = take_control::<RejoinRequest>(&mut a, &b_id);
    arrived(
        &mut b,
        &a_id,
        RejoinRequest {
            nonce: [0; 32],
            ..req
        },
    );
    assert_eq!(
        take_control::<RejoinDenied>(&mut b, &a_id),
        RejoinDenied::NonceMismatch
    );
    arrived(&mut b, &a_id, req);
    let accepted = take_control::<RejoinAccepted>(&mut b, &a_id);
    arrived(&mut a, &b_id, accepted);
    let sync_request = take_control::<RequestStateSync>(&mut a, &b_id);
    arrived(&mut b, &a_id, sync_request);
    let sync = take_control::<GameStateSync>(&mut b, &a_id);
    let sync = GameStateSync::decode(&sync.encode().unwrap()).unwrap();
    arrived(&mut a, &b_id, sync.clone());
    assert_eq!(a.world().resource::<Counter>().0, 8);
    assert!(
        a.world()
            .resource::<StateSyncSnapshotApplyBarrier>()
            .is_active()
    );
    let first_ack = take_control::<StateSyncAck>(&mut a, &b_id);
    a.world_mut().resource_mut::<Counter>().0 = 9;
    arrived(&mut a, &b_id, sync);
    let replay_ack = take_control::<StateSyncAck>(&mut a, &b_id);
    assert_eq!(replay_ack, first_ack);
    assert_eq!(a.world().resource::<Counter>().0, 9);
    arrived(&mut b, &a_id, replay_ack);
    a.update();
    assert!(
        a.world()
            .resource::<StateSyncSnapshotApplyBarrier>()
            .is_active()
    );
    a.update();
    b.update();
    assert!(
        !a.world()
            .resource::<StateSyncSnapshotApplyBarrier>()
            .is_active()
    );
    assert!(a.world().get_resource::<ReconnectionState>().is_none());
    assert!(b.world().get_resource::<ReconnectionState>().is_none());
    assert!(
        b.world()
            .resource::<StateSyncTracker>()
            .pending_sync()
            .is_none()
    );
    assert_eq!(
        *a.world().resource::<State<TransportHealth>>().get(),
        TransportHealth::Healthy
    );
    assert_eq!(
        *b.world().resource::<State<TransportHealth>>().get(),
        TransportHealth::Healthy
    );
}

#[test]
fn liveness_plugin_refreshes_on_authenticated_ping_and_reports_stall() {
    use bevy::time::TimeUpdateStrategy;
    use std::time::Duration;

    #[derive(Default, Resource)]
    struct Losses(Vec<DisconnectReason>);
    let ((mut a, _), (_, b_id)) = connected_pair(0, 0);
    record_control::<LivenessPingEvent>(&mut a);
    record_control::<LivenessPingAckEvent>(&mut a);
    a.init_resource::<Losses>().add_observer(
        |event: On<ConnectionLostEvent>, mut losses: ResMut<Losses>| {
            losses.0.push(event.0.clone());
        },
    );
    a.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(5)));
    a.update();
    assert_eq!(take_control::<LivenessPingEvent>(&mut a, &b_id).nonce, 1);
    arrived(&mut a, &b_id, LivenessPingEvent { nonce: 8 });
    assert_eq!(take_control::<LivenessPingAckEvent>(&mut a, &b_id).nonce, 8);
    a.update();
    a.update();
    arrived(&mut a, &b_id, LivenessPingEvent { nonce: 8 });
    assert!(a.world().resource::<Losses>().0.is_empty());
    a.update();
    assert!(matches!(
        &a.world().resource::<Losses>().0[..],
        [DisconnectReason::LivenessStalled {
            missed_intervals: 3,
            elapsed_secs: 15.
        }]
    ));
}

#[test]
fn commitment_plugin_reports_reveal_mismatch_and_retry_exhaustion() {
    use bevy::time::TimeUpdateStrategy;
    use std::time::Duration;

    #[derive(Default, Resource)]
    struct Faults(Vec<SessionFault>);
    fn watch(app: &mut App) {
        app.init_resource::<Faults>().add_observer(
            |fault: On<SessionFault>, mut faults: ResMut<Faults>| {
                faults.0.push(fault.event().clone());
            },
        );
    }
    let ((mut a, _), (mut b, b_id)) = connected_pair(0, 0);
    watch(&mut a);
    watch(&mut b);
    a.world_mut().trigger(BeginCommitment {
        turn: 1,
        payload: Tick(1),
    });
    arrived(
        &mut a,
        &b_id,
        CommitmentMessage::Commit {
            turn: 1,
            hash: [0; 32],
        },
    );
    arrived(
        &mut a,
        &b_id,
        CommitmentMessage::Reveal {
            turn: 1,
            payload: vec![9],
            nonce: [0; 32],
        },
    );
    assert_eq!(
        a.world().resource::<Faults>().0,
        [SessionFault::CommitmentMismatch]
    );
    b.world_mut().trigger(BeginCommitment {
        turn: 1,
        payload: Tick(1),
    });
    b.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));
    for _ in 0..4 {
        b.update();
    }
    assert_eq!(
        b.world().resource::<Faults>().0,
        [SessionFault::CommitmentRetriesExhausted]
    );
}

#[test]
fn mismatch_recovery_exhaustion_disconnects_as_desync() {
    use bevy::time::TimeUpdateStrategy;
    use std::time::Duration;

    #[derive(Default, Resource)]
    struct ResultLog {
        timeouts: Vec<RejoinStepTimeout>,
        faults: Vec<SessionFault>,
    }
    let ((mut a, _), (_, b_id)) = connected_pair(2, 2);
    record_control::<RequestStateSync>(&mut a);
    a.init_resource::<ResultLog>()
        .add_observer(|event: On<RejoinStepTimeout>, mut log: ResMut<ResultLog>| {
            log.timeouts.push(*event.event())
        })
        .add_observer(|event: On<SessionFault>, mut log: ResMut<ResultLog>| {
            log.faults.push(event.event().clone())
        });
    a.world_mut().trigger(StateHashMismatch((
        BattleNumber(1),
        RoundNumber(0),
        TurnNumber(2),
    )));
    let first = take_control::<RequestStateSync>(&mut a, &b_id);
    a.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));
    for _ in 0..7 {
        a.update();
    }
    assert_eq!(take_control::<RequestStateSync>(&mut a, &b_id), first);
    assert_eq!(take_control::<RequestStateSync>(&mut a, &b_id), first);
    let log = a.world().resource::<ResultLog>();
    assert_eq!(
        log.timeouts,
        [RejoinStepTimeout {
            step: RejoinStep::RequestStateSync,
            attempts: 3
        }]
    );
    assert_eq!(log.faults, [SessionFault::Desync]);
}
