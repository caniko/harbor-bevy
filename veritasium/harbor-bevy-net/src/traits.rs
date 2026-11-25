use bevy::prelude::World;
use bevy_replicated::{ChainHash, dedup::KindIndex, epoch::SessionEpoch};
use veritasium_wire::{OutboundSeq, PeerIdBytes};

use crate::{PendingEvent, SessionFault};

pub trait GameNet: Send + Sync + 'static {
    type Router: SessionRouter;
    type Packet: Packet;
    type Snapshot: SnapshotCodec;
    type Hidden: CommitPayload;
}

/// Application admission and validated decoding at the actor boundary.
/// Return `None` for traffic that should not be dispatched. Only return a
/// session-ending fault from `route` for a peer admitted to this session.
pub trait SessionRouter: Send + Sync + 'static {
    fn accepts_peer(&self, peer: &PeerIdBytes) -> bool;
    fn route(
        &mut self,
        from: PeerIdBytes,
        packet: &[u8],
    ) -> Result<Option<PendingEvent>, SessionFault>;
}

pub trait Packet: Sized + Send + Sync + 'static {
    type Kind: KindIndex + Send + Sync + 'static;
    type Error: std::error::Error + Send + Sync + 'static;

    fn kind(&self) -> Self::Kind;
    fn sequence(&self) -> OutboundSeq;
    fn epoch(&self) -> SessionEpoch;
    fn encode(&self) -> Result<Vec<u8>, Self::Error>;
    fn decode(bytes: &[u8]) -> Result<Self, Self::Error>;
}

pub trait SnapshotCodec: Sized + Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    fn capture(world: &World) -> Result<Self, Self::Error>;
    /// Apply a validated snapshot. On error the application must leave its
    /// state unchanged.
    fn apply(self, world: &mut World) -> Result<(), Self::Error>;
    fn progress(&self) -> crate::SnapshotProgress;
    fn chain_hash(&self) -> ChainHash;
    fn encode(&self) -> Result<Vec<u8>, Self::Error>;
    fn decode(bytes: &[u8]) -> Result<Self, Self::Error>;
}

pub trait CommitPayload: Sized + Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    fn to_commit_bytes(&self) -> Result<Vec<u8>, Self::Error>;
    fn from_commit_bytes(bytes: &[u8]) -> Result<Self, Self::Error>;
}
