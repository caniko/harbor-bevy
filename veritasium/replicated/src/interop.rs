//! Public convention surface for foreign transport adapters.
//!
//! Foreign transports do not implement a `bevy_replicated` trait. Instead they
//! map their own peer identity to [`ReplicationPeerId`], advertise which
//! [`ChannelKind`] values they can honor, drain outbound packet queues, and feed
//! inbound wire bytes back through this module. Unsupported required channel
//! kinds are errors, not reliability downgrades.

use bevy::prelude::World;

pub use crate::ChannelKind;
use crate::{
    ChainHashOutbox, ChainHashReportStatus, GossipOutbox, OrderingPolicy, ReceiveTransactionStatus,
    ReplicationParticipants, ReplicationPeerId, TransactionBuffer, TransactionWireId, WireError,
    decode_chain_hash_report, decode_transaction, receive_chain_hash_report,
};

/// Kind of replication wire payload carried by a foreign transport packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReplicationPacketKind {
    Transaction,
    ChainHashReport,
}

/// Byte packet ready for a foreign transport queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundReplicationPacket {
    pub channel: ChannelKind,
    pub kind: ReplicationPacketKind,
    pub payload: Vec<u8>,
}

/// Borrowed inbound packet decoded from a foreign transport queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboundReplicationPacket<'a> {
    pub from: ReplicationPeerId,
    pub channel: ChannelKind,
    pub kind: ReplicationPacketKind,
    pub payload: &'a [u8],
}

/// Channel kinds a foreign transport bridge can actually honor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelSupport {
    pub reliable_ordered: bool,
    pub reliable_unordered: bool,
    pub unreliable: bool,
    pub tick_buffered: bool,
}

impl ChannelSupport {
    pub const fn reliable_ordered_only() -> Self {
        Self {
            reliable_ordered: true,
            reliable_unordered: false,
            unreliable: false,
            tick_buffered: false,
        }
    }

    pub const fn supports(self, channel: ChannelKind) -> bool {
        match channel {
            ChannelKind::ReliableOrdered => self.reliable_ordered,
            ChannelKind::ReliableUnordered => self.reliable_unordered,
            ChannelKind::Unreliable => self.unreliable,
            ChannelKind::TickBuffered => self.tick_buffered,
        }
    }

    pub fn require(self, channel: ChannelKind) -> Result<(), InteropError> {
        if self.supports(channel) {
            Ok(())
        } else {
            Err(InteropError::UnsupportedChannel { required: channel })
        }
    }
}

/// Result of accepting an inbound foreign-transport packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundPacketStatus {
    Transaction(ReceiveTransactionStatus),
    ChainHashReport(ChainHashReportStatus),
}

/// Inbound operation that failed after the packet's wire bytes decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundReceiveOperation {
    Transaction,
    ChainHashReport,
}

#[derive(Debug)]
pub enum InteropError {
    UnsupportedChannel {
        required: ChannelKind,
    },
    PeerMismatch {
        packet_from: ReplicationPeerId,
        payload_peer: ReplicationPeerId,
    },
    Wire(WireError),
    Receive {
        operation: InboundReceiveOperation,
        reason: String,
    },
}

impl core::fmt::Display for InteropError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnsupportedChannel { required } => {
                write!(
                    f,
                    "foreign transport cannot honor {required:?} replication channel"
                )
            }
            Self::PeerMismatch {
                packet_from,
                payload_peer,
            } => write!(
                f,
                "foreign peer mapping mismatch: packet came from {packet_from:?}, payload names \
                 {payload_peer:?}"
            ),
            Self::Wire(error) => write!(f, "{error}"),
            Self::Receive { operation, reason } => {
                write!(f, "failed to receive {operation:?} packet: {reason}")
            }
        }
    }
}

impl std::error::Error for InteropError {}

impl From<WireError> for InteropError {
    fn from(error: WireError) -> Self {
        Self::Wire(error)
    }
}

/// Drains outbound transaction and chain-hash queues into transport packets.
///
/// The returned payload bytes are exactly the library's rkyv wire bytes. Bridge
/// crates may wrap them in their own framing, but must not reserialize the
/// chain-folded transaction payload in another format.
pub fn drain_outbound_packets<Id, O>(
    gossip: &mut GossipOutbox<Id>,
    chain_hashes: &mut ChainHashOutbox,
    supported_channels: ChannelSupport,
) -> Result<Vec<OutboundReplicationPacket>, InteropError>
where
    Id: TransactionWireId,
    O: OrderingPolicy,
{
    supported_channels.require(O::CHANNEL_KIND)?;
    let mut packets = Vec::new();

    packets.extend(
        gossip
            .drain()
            .into_iter()
            .map(|envelope| OutboundReplicationPacket {
                channel: O::CHANNEL_KIND,
                kind: ReplicationPacketKind::Transaction,
                payload: envelope.wire_bytes,
            }),
    );

    packets.extend(
        chain_hashes
            .drain()
            .into_iter()
            .map(|envelope| OutboundReplicationPacket {
                channel: O::CHANNEL_KIND,
                kind: ReplicationPacketKind::ChainHashReport,
                payload: envelope.wire_bytes,
            }),
    );

    Ok(packets)
}

/// Drains outbound transaction and chain-hash queues from a Bevy world.
///
/// This is the ergonomic equivalent of [`drain_outbound_packets`] for examples,
/// tests, and framework bridges that operate directly on an app/world.
pub fn drain_world_outbound_packets<Id, O>(
    world: &mut World,
    supported_channels: ChannelSupport,
) -> Result<Vec<OutboundReplicationPacket>, InteropError>
where
    Id: TransactionWireId,
    O: OrderingPolicy,
{
    world.resource_scope::<GossipOutbox<Id>, _>(|world, mut gossip| {
        world.resource_scope::<ChainHashOutbox, _>(|_world, mut chain_hashes| {
            drain_outbound_packets::<Id, O>(&mut gossip, &mut chain_hashes, supported_channels)
        })
    })
}

/// Decodes and applies one inbound transport packet.
///
/// `packet.from` is the bridge's foreign-peer-to-[`ReplicationPeerId`] mapping.
/// It must match the peer identity embedded in the replication payload.
pub fn receive_inbound_packet<Id>(
    world: &mut World,
    packet: InboundReplicationPacket<'_>,
    supported_channels: ChannelSupport,
) -> Result<InboundPacketStatus, InteropError>
where
    Id: TransactionWireId,
{
    supported_channels.require(packet.channel)?;

    match packet.kind {
        ReplicationPacketKind::Transaction => {
            let transaction = decode_transaction::<Id>(packet.payload)?;
            if transaction.peer_id != packet.from {
                return Err(InteropError::PeerMismatch {
                    packet_from: packet.from,
                    payload_peer: transaction.peer_id,
                });
            }
            let peer_id = transaction.peer_id;
            let status = world
                .resource_mut::<TransactionBuffer<Id>>()
                .receive_transaction_with_wire_bytes(transaction, packet.payload.to_vec())
                .map_err(|error| InteropError::Receive {
                    operation: InboundReceiveOperation::Transaction,
                    reason: error.to_string(),
                })?;
            world
                .resource_mut::<ReplicationParticipants>()
                .observe(peer_id);
            Ok(InboundPacketStatus::Transaction(status))
        }
        ReplicationPacketKind::ChainHashReport => {
            let report = decode_chain_hash_report(packet.payload)?;
            if report.peer() != packet.from {
                return Err(InteropError::PeerMismatch {
                    packet_from: packet.from,
                    payload_peer: report.peer(),
                });
            }
            Ok(InboundPacketStatus::ChainHashReport(
                receive_chain_hash_report(world, report),
            ))
        }
    }
}

/// Helper for convention-only bridges that need owned packets before borrowing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedInboundReplicationPacket {
    pub from: ReplicationPeerId,
    pub channel: ChannelKind,
    pub kind: ReplicationPacketKind,
    pub payload: Vec<u8>,
}

impl OwnedInboundReplicationPacket {
    pub fn as_borrowed(&self) -> InboundReplicationPacket<'_> {
        InboundReplicationPacket {
            from: self.from,
            channel: self.channel,
            kind: self.kind,
            payload: &self.payload,
        }
    }
}
