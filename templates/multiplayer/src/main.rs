//! In-process equal-peer demo. For live sessions, establish the match epoch and
//! pass an admitted-peer router and ingress handle to `harbor_bevy_net::spawn_session`.

use std::io::{Error, ErrorKind};

use bevy::prelude::*;
use bevy_replicated::{
    EntityReplicas, Mutation, OpaqueId, Replicated, ReplicatedAppExt, ReplicatedComponent,
    ReplicatedPlugin, ReplicationIdentity, ReplicationPeerId, Transaction, dedup::KindIndex,
    receive_transaction, without_replication_capture,
};
use harbor_bevy::{HarborCorePlugin, HarborUiPlugin};
use harbor_bevy_net::{
    AppObserverOrderingExt, BattleNumber, ChainHash, CommitPayload, FromNetwork, GameNet,
    HarborNetPlugins, IngressLane, LocalEpoch, NetConfig, OutboundSeq, Packet, PeerIdBytes,
    PendingEvent, RoundNumber, SessionEpoch, SessionFault, SessionIngressHandle, SessionRouter,
    SnapshotCodec, SnapshotProgress, TurnNumber,
};

const SCORE_ID: OpaqueId = OpaqueId(1);
const PEER_DOMAIN: &[u8] = b"harbor-multiplayer";
const DEMO_EPOCH: SessionEpoch =
    SessionEpoch::established([1; 16], BattleNumber(1), RoundNumber(0));

struct MyGame;

impl GameNet for MyGame {
    type Router = Router;
    type Packet = ScorePacket;
    type Snapshot = Snapshot;
    type Hidden = Hidden;
}

struct Router(PeerIdBytes);

impl SessionRouter for Router {
    fn accepts_peer(&self, peer: &PeerIdBytes) -> bool {
        peer == &self.0
    }

    fn route(
        &mut self,
        from: PeerIdBytes,
        bytes: &[u8],
    ) -> Result<Option<PendingEvent>, SessionFault> {
        if !self.accepts_peer(&from) {
            return Ok(None);
        }
        let packet = ScorePacket::decode(bytes)
            .map_err(|error| SessionFault::ValidationViolation(error.to_string()))?;
        Ok((packet.epoch() == DEMO_EPOCH)
            .then(|| PendingEvent::new(IngressLane::Reliable, Some(from), packet)))
    }
}

#[derive(Clone, Copy)]
struct ScoreKind;

impl KindIndex for ScoreKind {
    const COUNT: usize = 1;

    fn kind_index(self) -> usize {
        0
    }
}

#[derive(Clone, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct ScorePacket {
    epoch: SessionEpoch,
    sequence: u64,
    score: Score,
}

impl Packet for ScorePacket {
    type Kind = ScoreKind;
    type Error = Error;

    fn kind(&self) -> ScoreKind {
        ScoreKind
    }

    fn sequence(&self) -> OutboundSeq {
        OutboundSeq(self.sequence)
    }

    fn epoch(&self) -> SessionEpoch {
        self.epoch
    }

    fn encode(&self) -> Result<Vec<u8>, Error> {
        rkyv::to_bytes::<rkyv::rancor::Error>(self)
            .map(|bytes| bytes.to_vec())
            .map_err(Error::other)
    }

    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != size_of::<ArchivedScorePacket>() {
            return Err(ErrorKind::InvalidData.into());
        }
        let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(bytes.len());
        aligned.extend_from_slice(bytes);
        rkyv::from_bytes::<Self, rkyv::rancor::Error>(&aligned)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))
    }
}

#[derive(Clone, Replicated, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[replicated(component_id = 1)]
struct Score(u32);

struct Snapshot {
    score: Score,
}

impl SnapshotCodec for Snapshot {
    type Error = Error;

    fn capture(world: &World) -> Result<Self, Error> {
        let entity = score_entity(world)?;
        world
            .get::<Score>(entity)
            .cloned()
            .map(|score| Self { score })
            .ok_or_else(|| ErrorKind::NotFound.into())
    }

    fn apply(self, world: &mut World) -> Result<(), Error> {
        let entity = score_entity(world)?;
        without_replication_capture(world, |world| {
            world
                .get_entity_mut(entity)
                .map_err(|_| ErrorKind::NotFound)?
                .insert(self.score);
            Ok(())
        })
    }

    fn progress(&self) -> SnapshotProgress {
        (
            DEMO_EPOCH.battle_number,
            DEMO_EPOCH.round_number,
            TurnNumber(0),
        )
    }

    fn chain_hash(&self) -> ChainHash {
        // This demo hashes snapshot state; it does not restore replication history.
        *blake3::hash(&self.score.0.to_le_bytes()).as_bytes()
    }

    fn encode(&self) -> Result<Vec<u8>, Error> {
        Ok(self.score.0.to_le_bytes().to_vec())
    }

    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        Ok(Self {
            score: Score(decode_u32(bytes)?),
        })
    }
}

fn score_entity(world: &World) -> Result<Entity, Error> {
    world
        .get_resource::<EntityReplicas<OpaqueId>>()
        .and_then(|replicas| replicas.entity(SCORE_ID))
        .ok_or_else(|| ErrorKind::NotFound.into())
}

struct Hidden(u32);

impl CommitPayload for Hidden {
    type Error = Error;

    fn to_commit_bytes(&self) -> Result<Vec<u8>, Error> {
        Ok(self.0.to_le_bytes().to_vec())
    }

    fn from_commit_bytes(bytes: &[u8]) -> Result<Self, Error> {
        Ok(Self(decode_u32(bytes)?))
    }
}

fn decode_u32(bytes: &[u8]) -> Result<u32, Error> {
    let bytes = bytes.try_into().map_err(|_| ErrorKind::InvalidData)?;
    Ok(u32::from_le_bytes(bytes))
}

fn new_peer(id: &PeerIdBytes) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        HarborNetPlugins::<MyGame>::new(NetConfig {
            app_id: "harbor-multiplayer".into(),
            protocol_prefix: "/harbor-multiplayer".into(),
            ..default()
        }),
        HarborCorePlugin,
        HarborUiPlugin,
        ReplicatedPlugin::<OpaqueId>::default(),
    ));
    app.insert_resource(LocalEpoch(DEMO_EPOCH))
        .insert_resource(ReplicationIdentity {
            peer_id: ReplicationPeerId::from_stable_bytes(PEER_DOMAIN, id.as_slice()),
        });
    <App as ReplicatedAppExt<OpaqueId>>::register_replicated_component::<Score>(&mut app)
        .add_network_bridge_observer(receive_score);
    let entity = app.world_mut().spawn(Score(0)).id();
    app.world_mut()
        .resource_mut::<EntityReplicas<OpaqueId>>()
        .bind(entity, SCORE_ID);
    app
}

fn receive_score(event: On<FromNetwork<ScorePacket>>, mut commands: Commands) {
    let Some(from) = &event.from else { return };
    let peer = ReplicationPeerId::from_stable_bytes(PEER_DOMAIN, from.as_slice());
    let packet = &event.inner;
    let payload = packet
        .score
        .encode_payload()
        .expect("encode a fixed-size score");
    let mutation = Mutation::modify(SCORE_ID, Score::COMPONENT_ID, payload);
    let transaction = Transaction::new(peer, packet.sequence().0, vec![mutation]);
    commands.queue(move |world: &mut World| {
        if let Err(error) = receive_transaction(world, transaction) {
            bevy::log::warn!("score transaction rejected: {error}");
        }
    });
}

fn exchange_score(app: &mut App, router: &mut Router, from: PeerIdBytes, score: u32) {
    let bytes = ScorePacket {
        epoch: DEMO_EPOCH,
        sequence: 0,
        score: Score(score),
    }
    .encode()
    .unwrap();
    let event = router.route(from, &bytes).unwrap().unwrap();
    app.world()
        .resource::<SessionIngressHandle>()
        .push(event)
        .unwrap();
    // Ingress and replication both run in PreUpdate without an ordering edge.
    app.update();
    app.update();
}

fn main() {
    let a = PeerIdBytes::from([1]);
    let b = PeerIdBytes::from([2]);
    let mut first = new_peer(&a);
    let mut second = new_peer(&b);
    exchange_score(&mut first, &mut Router(b.clone()), b, 5);
    exchange_score(&mut second, &mut Router(a.clone()), a, 7);
    let first_score = Snapshot::capture(first.world()).unwrap().score.0;
    let second_score = Snapshot::capture(second.world()).unwrap().score.0;
    println!("Peer one received {first_score}; peer two received {second_score}");
}

#[cfg(test)]
#[test]
fn equal_peers_route_replicate_and_restore() {
    let a = PeerIdBytes::from([1]);
    let b = PeerIdBytes::from([2]);
    let mut first = new_peer(&a);
    let mut second = new_peer(&b);
    let mut first_router = Router(b.clone());
    assert!(first_router.route(a.clone(), &[]).unwrap().is_none());
    assert!(first_router.route(b.clone(), &[]).is_err());

    exchange_score(&mut first, &mut first_router, b, 5);
    exchange_score(&mut second, &mut Router(a.clone()), a, 7);
    let snapshot = Snapshot::capture(first.world()).unwrap();
    assert_eq!(snapshot.score.0, 5);
    assert_eq!(Snapshot::capture(second.world()).unwrap().score.0, 7);
    assert_eq!(
        first
            .world()
            .resource::<bevy_replicated::ReplicationChain>()
            .count(),
        1
    );

    let copy = Snapshot::decode(&snapshot.encode().unwrap()).unwrap();
    assert_eq!(copy.chain_hash(), snapshot.chain_hash());
    copy.apply(second.world_mut()).unwrap();
    second.update();
    second.update();
    assert_eq!(Snapshot::capture(second.world()).unwrap().score.0, 5);
    assert_eq!(
        second
            .world()
            .resource::<bevy_replicated::ReplicationChain>()
            .count(),
        1,
        "restoring a snapshot must not emit a local transaction"
    );
    assert_eq!(
        Hidden::from_commit_bytes(&Hidden(3).to_commit_bytes().unwrap())
            .unwrap()
            .0,
        3
    );
}
