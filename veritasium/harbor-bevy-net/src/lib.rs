//! Equal-peer networking for Veritasium Bevy applications.

use std::marker::PhantomData;

use bevy::app::{App, Plugin, PluginGroup, PluginGroupBuilder};

pub mod admission;
pub mod commitment;
pub mod config;
pub mod discovery;
pub mod fatal;
pub mod gossip;
pub mod lifecycle;
pub mod liveness;
pub mod mesh;
pub mod peer_id;
pub mod reliable;
pub mod rng;
pub mod session;
pub mod state_sync;
pub mod trace;
pub mod traits;

pub use admission::*;
pub use bevy_replicated::{
    ChainHash,
    epoch::{LocalEpoch, SessionEpoch},
};
pub use commitment::*;
pub use config::{NetConfig, Transports};
pub use discovery::*;
pub use fatal::NetFatal;
pub use gossip::*;
pub use harbor_bevy_core::{
    fatal::GracefulShutdownPlugin,
    observer_ordering::{
        AppObserverOrderingExt, GameplayMutationObservers, NetworkBridgeObservers,
        ObserverOrderingPlugin, ValidationObservers,
    },
};
pub use lifecycle::*;
pub use liveness::*;
pub use mesh::*;
pub use peer_id::*;
pub use reliable::*;
pub use rng::*;
pub use session::*;
pub use state_sync::*;
pub use thespan::{GossipTopic, Multiaddr, PeerId};
pub use thespan_bevy::{FromNetwork, RemoteSendFailedEvent, ThespanBevyPlugin};
pub use traits::*;
pub use veritasium_wire::{
    BattleNumber, OutboundSeq, PeerIdBytes, PlayerId, RoundNumber, TurnNumber,
};

pub struct HarborNetPlugins<G: GameNet> {
    config: NetConfig,
    game: PhantomData<fn() -> G>,
}

impl<G: GameNet> HarborNetPlugins<G> {
    pub fn new(config: NetConfig) -> Self {
        Self {
            config,
            game: PhantomData,
        }
    }
}

struct ConfigureNet(NetConfig);

impl Plugin for ConfigureNet {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.0.clone());
    }
}

impl<G: GameNet> PluginGroup for HarborNetPlugins<G> {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(ConfigureNet(self.config))
            .add(ObserverOrderingPlugin)
            .add(GracefulShutdownPlugin)
            .add(ThespanBevyPlugin)
            .add(ConnectionLifecyclePlugin)
            .add(PeerNegotiationPlugin)
            .add(SessionPlugin::<G>::default())
            .add(ReliablePlugin::<G>::default())
            .add(GossipPlugin)
            .add(LivenessPlugin)
            .add(StateSyncPlugin::<G>::default())
            .add(MeshPlugin)
            .add(AdmissionPlugin)
            .add(CommitmentPlugin::<G>::default())
    }
}
