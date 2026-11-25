//! Peer connection tickets — what a discoverer needs to dial an announcer.

/// Opaque libp2p `PeerId` encoded as a multihash byte string.
///
/// Carried as bytes so this crate does not depend on `libp2p-identity`.
/// Clients/servers convert via `PeerId::from_bytes` / `PeerId::to_bytes`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct PeerIdBytes(pub Vec<u8>);

impl PeerIdBytes {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

/// Multiaddr in canonical string form (e.g. `/ip4/1.2.3.4/udp/4001/quic-v1`).
///
/// String form chosen over the binary form so the protocol is human-debuggable
/// in tcpdump and remains stable across libp2p versions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct MultiaddrBytes(pub String);

/// Hint that a peer is reachable through a Circuit Relay v2 hop.
///
/// The `relay_peer` field names the relay's peer id; `relay_addrs` are the
/// dialable addresses *of the relay*, not the announcer. The dialer
/// constructs the full circuit address itself
/// (`<relay_addr>/p2p/<relay_peer>/p2p-circuit/p2p/<announcer>`).
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct RelayHint {
    pub relay_peer: PeerIdBytes,
    pub relay_addrs: Vec<MultiaddrBytes>,
}

/// Everything a discoverer needs to dial an announcing peer.
///
/// `addrs` SHOULD include at least one address each peer would observe
/// (loopback, LAN, plus any public/relay address learned via `AutoNAT`).
/// Order is not significant; clients try in parallel.
#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct PeerTicket {
    pub peer_id: PeerIdBytes,
    pub addrs: Vec<MultiaddrBytes>,
    pub relay_hints: Vec<RelayHint>,
}
