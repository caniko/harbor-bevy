//! Shape-A dogfooding consumer for `bevy_replicated`.
//!
//! This standalone example models a tiny turn-based grid painter using
//! `OpaqueId` identities rather than any game-specific network id.

use bevy::prelude::*;
use bevy_replicated::ReliableOrdered;
use bevy_replicated::interop::{
    ChannelSupport, InboundReplicationPacket, ReplicationPacketKind, drain_world_outbound_packets,
    receive_inbound_packet,
};
use bevy_replicated::{
    AppliedTransactions, ComponentId, EntityReplicas, OpaqueId, ReplicatedAppExt,
    ReplicatedComponent, ReplicatedPlugin, ReplicationChain, ReplicationIdentity,
    ReplicationPeerId, Transaction, WireError,
};

const CELL_COMPONENT_ID: ComponentId = ComponentId(70_001);
const CELL_ID_BASE: u64 = 1_000;
const PEER_A: ReplicationPeerId = ReplicationPeerId([0xA1; 32]);
const PEER_B: ReplicationPeerId = ReplicationPeerId([0xB2; 32]);

#[derive(
    bevy::ecs::component::MediatedAccess,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
struct CellPaint {
    color: u8,
    painter: u8,
}

impl ReplicatedComponent for CellPaint {
    const COMPONENT_ID: ComponentId = CELL_COMPONENT_ID;

    fn encode_payload(&self) -> Result<Vec<u8>, WireError> {
        rkyv::to_bytes::<rkyv::rancor::Error>(self)
            .map(|bytes| bytes.to_vec())
            .map_err(WireError::Encode)
    }

    fn decode_payload(bytes: &[u8]) -> Result<Self, WireError> {
        rkyv::from_bytes::<Self, rkyv::rancor::Error>(bytes).map_err(WireError::Decode)
    }
}

#[derive(Debug, Clone, Copy)]
struct PaintCommand {
    cell: OpaqueId,
    color: u8,
    painter: u8,
}

#[derive(Resource, Default)]
struct PaintQueue(Vec<PaintCommand>);

pub fn run_shape_a_demo() {
    let mut peer_a = painter_peer(PEER_A);
    let mut peer_b = painter_peer(PEER_B);

    enqueue_paint(&mut peer_a, 0, 4, 1);
    enqueue_paint(&mut peer_b, 8, 9, 2);

    run_local_turn(&mut peer_a);
    run_local_turn(&mut peer_b);

    exchange_gossip(&mut peer_a, &mut peer_b);
    exchange_gossip(&mut peer_b, &mut peer_a);

    apply_received(&mut peer_a);
    apply_received(&mut peer_b);

    assert_eq!(chain_hash(&peer_a), chain_hash(&peer_b));
    assert_eq!(painted_cells(&peer_a), painted_cells(&peer_b));
    assert_eq!(applied_count(&peer_a), 2);
    assert_eq!(applied_count(&peer_b), 2);
}

pub fn wire_reference_hashes() -> (&'static str, &'static str) {
    (
        "d65fe7e0dc872035a1e825ae6238aba95ac6b8c40dfbdaf30fb3a37e3831211d",
        "d98195fb0aefd592a26e6274f43b78f77db502494499f5c84e4ae16c022810e5",
    )
}

pub fn assert_wire_reference_vectors() {
    let payload = CellPaint {
        color: 4,
        painter: 1,
    }
    .encode_payload()
    .expect("cell paint encodes");
    let transaction = Transaction::new(
        PEER_A,
        0,
        vec![bevy_replicated::Mutation::modify(
            cell_id(0),
            CELL_COMPONENT_ID,
            payload.clone(),
        )],
    );
    let wire_bytes =
        bevy_replicated::encode_transaction(&transaction).expect("transaction encodes");
    let (expected_payload_hash, expected_wire_hash) = wire_reference_hashes();

    assert_eq!(hex_hash(&payload), expected_payload_hash);
    assert_eq!(hex_hash(&wire_bytes), expected_wire_hash);
}

fn painter_peer(peer_id: ReplicationPeerId) -> App {
    let mut app = App::new();
    app.add_plugins(ReplicatedPlugin::<OpaqueId>::default());
    <App as ReplicatedAppExt<OpaqueId>>::register_replicated_component::<CellPaint>(&mut app);
    app.world_mut()
        .insert_resource(ReplicationIdentity { peer_id });
    app.world_mut().insert_resource(PaintQueue::default());
    app.add_systems(Update, apply_paint_queue);

    for index in 0..9 {
        let entity = app
            .world_mut()
            .spawn(CellPaint {
                color: 0,
                painter: 0,
            })
            .id();
        app.world_mut()
            .resource_mut::<EntityReplicas<OpaqueId>>()
            .bind(entity, cell_id(index));
    }

    app
}

fn apply_paint_queue(
    mut queue: ResMut<PaintQueue>,
    replicas: Res<EntityReplicas<OpaqueId>>,
    mut cells: bevy::ecs::system::MediatedMut<CellPaint>,
) {
    let commands = core::mem::take(&mut queue.0);
    for command in commands {
        let entity = replicas
            .entity(command.cell)
            .expect("paint command targets a bound cell");
        cells
            .modify(entity, |cell| {
                cell.color = command.color;
                cell.painter = command.painter;
            })
            .expect("paint command targets a cell with CellPaint");
    }
}

fn enqueue_paint(app: &mut App, cell: u64, color: u8, painter: u8) {
    app.world_mut()
        .resource_mut::<PaintQueue>()
        .0
        .push(PaintCommand {
            cell: cell_id(cell),
            color,
            painter,
        });
}

fn run_local_turn(app: &mut App) {
    app.update();
}

fn exchange_gossip(sender: &mut App, receiver: &mut App) {
    let packets = drain_world_outbound_packets::<OpaqueId, ReliableOrdered>(
        sender.world_mut(),
        ChannelSupport::reliable_ordered_only(),
    )
    .expect("grid painter transport supports reliable ordered replication");

    for packet in packets {
        if packet.kind != ReplicationPacketKind::Transaction {
            continue;
        }
        let from = packet_peer(&packet.payload);
        receive_inbound_packet::<OpaqueId>(
            receiver.world_mut(),
            InboundReplicationPacket {
                from,
                channel: packet.channel,
                kind: packet.kind,
                payload: &packet.payload,
            },
            ChannelSupport::reliable_ordered_only(),
        )
        .expect("transaction packet received");
    }
}

fn packet_peer(payload: &[u8]) -> ReplicationPeerId {
    bevy_replicated::decode_transaction::<OpaqueId>(payload)
        .expect("transaction decodes")
        .peer_id
}

fn apply_received(app: &mut App) {
    app.update();
}

fn chain_hash(app: &App) -> bevy_replicated::ChainHash {
    app.world().resource::<ReplicationChain>().current()
}

fn applied_count(app: &App) -> usize {
    app.world()
        .resource::<AppliedTransactions<OpaqueId>>()
        .transactions()
        .len()
}

fn painted_cells(app: &App) -> Vec<(u8, u8)> {
    (0..9)
        .map(|index| {
            let entity = app
                .world()
                .resource::<EntityReplicas<OpaqueId>>()
                .entity(cell_id(index))
                .expect("cell exists");
            let paint = app.world().get::<CellPaint>(entity).expect("paint exists");
            (paint.color, paint.painter)
        })
        .collect()
}

fn cell_id(index: u64) -> OpaqueId {
    OpaqueId::new(CELL_ID_BASE + index)
}

fn hex_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_derives_round_trip_payloads() {
        #[derive(
            bevy_replicated::Replicated, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
        )]
        #[replicated(component_id = 70_002)]
        struct DerivedCell(u8);

        #[derive(
            bevy_replicated::ReplicatedResource,
            Clone,
            rkyv::Archive,
            rkyv::Serialize,
            rkyv::Deserialize,
        )]
        #[replicated(resource_id = 70_003)]
        struct DerivedPalette(u8);

        use bevy_replicated::ReplicatedResource as _;

        assert_eq!(DerivedCell::COMPONENT_ID, ComponentId(70_002));
        assert_eq!(
            DerivedPalette::RESOURCE_ID,
            bevy_replicated::ResourceId(70_003)
        );
        let cell = DerivedCell(4).encode_payload().expect("component encodes");
        let palette = DerivedPalette(9)
            .encode_payload()
            .expect("resource encodes");
        assert_eq!(
            DerivedCell::decode_payload(&cell)
                .expect("component decodes")
                .0,
            4
        );
        assert_eq!(
            DerivedPalette::decode_payload(&palette)
                .expect("resource decodes")
                .0,
            9
        );
    }

    #[test]
    fn two_peer_grid_painter_converges() {
        run_shape_a_demo();
    }

    #[test]
    fn wire_payloads_match_reference_vectors() {
        assert_wire_reference_vectors();
    }
}
