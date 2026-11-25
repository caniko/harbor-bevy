use bevy::prelude::*;
use bevy_replicated::{
    AppliedTransactions, AuthorityOrdered, AuthorityOrderedProposals, AuthorityPeer, ComponentId,
    EntityReplicas, Mutation, OpaqueId, ReliableOrdered, ReplicatedAppExt, ReplicatedComponent,
    ReplicatedPlugin, ReplicationChain, ReplicationPeerId, Transaction, WireError,
    receive_transaction,
};

const COMPONENT_ID: ComponentId = ComponentId(42);
const ENTITY_ID: OpaqueId = OpaqueId(1);
const AUTHORITY: ReplicationPeerId = ReplicationPeerId([1; 32]);
const READER_A: ReplicationPeerId = ReplicationPeerId([2; 32]);
const READER_B: ReplicationPeerId = ReplicationPeerId([3; 32]);

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
struct Position(u32);

impl ReplicatedComponent for Position {
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

fn main() {
    let mut app = App::new();
    app.add_plugins(ReplicatedPlugin::<
        OpaqueId,
        ReliableOrdered,
        AuthorityOrdered,
    >::default());
    <App as ReplicatedAppExt<OpaqueId>>::register_replicated_component::<Position>(&mut app);
    app.world_mut()
        .insert_resource(AuthorityPeer::new(AUTHORITY));

    let entity = app.world_mut().spawn(Position(0)).id();
    app.world_mut()
        .resource_mut::<EntityReplicas<OpaqueId>>()
        .bind(entity, ENTITY_ID);

    receive_transaction(
        app.world_mut(),
        Transaction::new(READER_A, 0, vec![set_position(99)]),
    )
    .expect("reader A proposal buffers");
    receive_transaction(
        app.world_mut(),
        Transaction::new(READER_B, 0, vec![set_position(123)]),
    )
    .expect("reader B proposal buffers");
    receive_transaction(
        app.world_mut(),
        Transaction::new(AUTHORITY, 0, vec![set_position(7)]),
    )
    .expect("authority transaction buffers");

    app.update();

    let position = app
        .world()
        .get::<Position>(entity)
        .expect("position exists")
        .0;
    let applied = app
        .world()
        .resource::<AppliedTransactions<OpaqueId>>()
        .transactions()
        .len();
    let proposals = app
        .world()
        .resource::<AuthorityOrderedProposals<OpaqueId>>()
        .proposals()
        .len();
    let chain_count = app.world().resource::<ReplicationChain>().count();

    println!(
        "position={position} applied={applied} proposals={proposals} chain_count={chain_count}"
    );
}

fn set_position(value: u32) -> Mutation<OpaqueId> {
    Mutation::modify(
        ENTITY_ID,
        COMPONENT_ID,
        Position(value).encode_payload().expect("position encodes"),
    )
}
