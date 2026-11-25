use std::collections::HashSet;

use bevy::prelude::*;

use crate::{ChainHash, FromNetwork, PeerId, PeerIdBytes, SendControl, SessionCleanupRequested};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Resource)]
pub enum PeerRole {
    #[default]
    Player,
    Observer,
}

#[derive(Debug, Default, Resource)]
pub struct ObserverPeers(pub HashSet<PeerId>);

/// Set by the application to open admission for an established match.
#[derive(Debug, Clone, Copy, Resource)]
pub struct ObserverAdmission {
    pub match_id: [u8; 16],
    pub genesis_hash: ChainHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ObserverHello {
    pub peer_id_bytes: PeerIdBytes,
    pub witness_id: [u8; 16],
    pub public_key: [u8; 32],
    pub name: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Event, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct ObserverAccepted {
    pub match_id: [u8; 16],
    pub genesis_hash: ChainHash,
}

pub struct AdmissionPlugin;

impl Plugin for AdmissionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PeerRole>()
            .init_resource::<ObserverPeers>()
            .add_observer(hello)
            .add_observer(
                |_: On<SessionCleanupRequested>,
                 mut peers: ResMut<ObserverPeers>,
                 mut commands: Commands| {
                    peers.0.clear();
                    commands.remove_resource::<ObserverAdmission>();
                },
            );
    }
}

fn hello(
    event: On<FromNetwork<ObserverHello>>,
    role: Res<PeerRole>,
    admission: Option<Res<ObserverAdmission>>,
    mut peers: ResMut<ObserverPeers>,
    mut commands: Commands,
) {
    let Some(admission) = admission.filter(|_| *role == PeerRole::Player) else {
        return;
    };
    // A sender-less router must already have verified the claimed identity.
    // A present but invalid transport sender never falls back to the claim.
    let from = event.from.as_ref().unwrap_or(&event.inner.peer_id_bytes);
    let Ok(peer) = PeerId::from_bytes(from.as_slice()) else {
        return;
    };
    peers.0.insert(peer);
    commands.trigger(SendControl::new(
        from.clone(),
        ObserverAccepted {
            match_id: admission.match_id,
            genesis_hash: admission.genesis_hash,
        },
    ));
}
