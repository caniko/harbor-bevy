//! Bevy-free wire-identity newtypes for the veritasium replication stack.
//!
//! Every type here is `#[repr(transparent)]` over its primitive and derives
//! rkyv so it can sit directly in wire payloads. Keeping this crate free of
//! Bevy (and everything else heavy) lets non-game binaries depend on the
//! session vocabulary without dragging in an ECS.
//!
//! Seat semantics: `PlayerId` is a *seat index* decided by negotiation
//! (coin flip / pairing data). Both peers are equal — there is no
//! host/client asymmetry in this vocabulary.

use smallvec::SmallVec;

/// Seat identifier used in gameplay and replication payloads.
///
/// `PlayerId` is a negotiated seat index, not a transport role. `ZERO` and
/// `ONE` identify the two gameplay sides after peer-id negotiation; neither
/// side has gameplay authority over the other.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    derive_more::Display,
    derive_more::From,
    derive_more::Into,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[display("{_0}")]
#[rkyv(derive(Debug))]
#[repr(transparent)]
pub struct PlayerId(pub u8);

impl PlayerId {
    /// Gameplay seat 0.
    pub const ZERO: Self = Self(0);
    /// Gameplay seat 1.
    pub const ONE: Self = Self(1);

    /// Convert this seat id to an array/vector index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Return the raw wire value.
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }

    /// Return the other two-player gameplay seat.
    ///
    /// Values other than 0 are treated as seat 1 for legacy compatibility, so
    /// callers should validate external input before constructing arbitrary ids.
    #[must_use]
    pub const fn other(self) -> Self {
        Self(if self.0 == 0 { 1 } else { 0 })
    }
}

impl PartialEq<u8> for PlayerId {
    fn eq(&self, other: &u8) -> bool {
        self.0 == *other
    }
}

impl PartialEq<PlayerId> for u8 {
    fn eq(&self, other: &PlayerId) -> bool {
        *self == other.0
    }
}

/// Compact serialized peer identity bytes used at Bevy-free boundaries.
///
/// The wrapper keeps libp2p/thespan peer identifiers out of the shared wire
/// crate while preserving their byte representation for archived payloads.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct PeerIdBytes(pub SmallVec<[u8; 38]>);

impl PeerIdBytes {
    /// Expected inline capacity for current libp2p peer-id byte encodings.
    pub const MAX_LEN: usize = 38;

    /// Borrow the encoded peer-id bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// Number of encoded bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no peer identity bytes have been stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<[u8]> for PeerIdBytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl From<Vec<u8>> for PeerIdBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(SmallVec::from_vec(bytes))
    }
}

impl From<&[u8]> for PeerIdBytes {
    fn from(bytes: &[u8]) -> Self {
        Self(SmallVec::from_slice(bytes))
    }
}

impl<const N: usize> From<[u8; N]> for PeerIdBytes {
    fn from(bytes: [u8; N]) -> Self {
        Self(SmallVec::from_slice(&bytes))
    }
}

impl rkyv::Archive for PeerIdBytes {
    type Archived = rkyv::vec::ArchivedVec<u8>;
    type Resolver = rkyv::vec::VecResolver;

    fn resolve(&self, resolver: Self::Resolver, out: rkyv::Place<Self::Archived>) {
        rkyv::vec::ArchivedVec::resolve_from_slice(self.as_slice(), resolver, out);
    }
}

impl<S> rkyv::Serialize<S> for PeerIdBytes
where
    S: rkyv::rancor::Fallible + rkyv::ser::Allocator + rkyv::ser::Writer + ?Sized,
{
    fn serialize(&self, serializer: &mut S) -> Result<Self::Resolver, S::Error> {
        rkyv::vec::ArchivedVec::<u8>::serialize_from_slice(self.as_slice(), serializer)
    }
}

impl<D> rkyv::Deserialize<PeerIdBytes, D> for rkyv::vec::ArchivedVec<u8>
where
    D: rkyv::rancor::Fallible + ?Sized,
{
    fn deserialize(&self, _deserializer: &mut D) -> Result<PeerIdBytes, D::Error> {
        Ok(PeerIdBytes::from(self.as_slice()))
    }
}

/// Monotonic battle counter used in replicated session epochs and gossip keys.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    derive_more::Display,
    derive_more::From,
    derive_more::Into,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[display("{_0}")]
#[rkyv(derive(Debug))]
#[repr(transparent)]
pub struct BattleNumber(pub u32);

impl BattleNumber {
    /// Return the raw wire value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl PartialEq<u32> for BattleNumber {
    fn eq(&self, other: &u32) -> bool {
        self.0 == *other
    }
}

impl PartialEq<BattleNumber> for u32 {
    fn eq(&self, other: &BattleNumber) -> bool {
        *self == other.0
    }
}

/// Round number within a battle progression.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    derive_more::Display,
    derive_more::From,
    derive_more::Into,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[display("{_0}")]
#[rkyv(derive(Debug))]
#[repr(transparent)]
pub struct RoundNumber(pub u32);

impl RoundNumber {
    /// Return the raw wire value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl PartialEq<u32> for RoundNumber {
    fn eq(&self, other: &u32) -> bool {
        self.0 == *other
    }
}

impl PartialEq<RoundNumber> for u32 {
    fn eq(&self, other: &RoundNumber) -> bool {
        *self == other.0
    }
}

/// Per-peer outbound sequence number for ordered replicated messages.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    derive_more::Display,
    derive_more::From,
    derive_more::Into,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[display("{_0}")]
#[rkyv(derive(Debug))]
#[repr(transparent)]
pub struct OutboundSeq(pub u64);

impl OutboundSeq {
    /// Return the raw wire value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl PartialEq<u64> for OutboundSeq {
    fn eq(&self, other: &u64) -> bool {
        self.0 == *other
    }
}

impl PartialEq<OutboundSeq> for u64 {
    fn eq(&self, other: &OutboundSeq) -> bool {
        *self == other.0
    }
}

/// Turn counter within a battle/round hash boundary.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    derive_more::Display,
    derive_more::From,
    derive_more::Into,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[display("{_0}")]
#[rkyv(derive(Debug))]
#[repr(transparent)]
pub struct TurnNumber(pub u32);

impl TurnNumber {
    /// Return the raw wire value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl PartialEq<u32> for TurnNumber {
    fn eq(&self, other: &u32) -> bool {
        self.0 == *other
    }
}

impl PartialEq<TurnNumber> for u32 {
    fn eq(&self, other: &TurnNumber) -> bool {
        *self == other.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn player_id_round_trips_through_rkyv() {
        let value = PlayerId::ONE;
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&value).expect("encode player id");
        let decoded =
            rkyv::from_bytes::<PlayerId, rkyv::rancor::Error>(&bytes).expect("decode player id");
        assert_eq!(decoded, value);
    }

    #[test]
    fn peer_id_bytes_round_trips_through_rkyv() {
        let value = PeerIdBytes::from([1, 2, 3, 4, 5, 6]);
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&value).expect("encode peer id bytes");
        let decoded = rkyv::from_bytes::<PeerIdBytes, rkyv::rancor::Error>(&bytes)
            .expect("decode peer id bytes");
        assert_eq!(decoded, value);
    }

    #[test]
    fn repr_transparent_keeps_wire_sizes() {
        assert_eq!(size_of::<PlayerId>(), size_of::<u8>());
        assert_eq!(size_of::<BattleNumber>(), size_of::<u32>());
        assert_eq!(size_of::<RoundNumber>(), size_of::<u32>());
        assert_eq!(size_of::<TurnNumber>(), size_of::<u32>());
        assert_eq!(size_of::<OutboundSeq>(), size_of::<u64>());
        assert_eq!(
            size_of::<rkyv::Archived<PlayerId>>(),
            size_of::<rkyv::Archived<u8>>()
        );
        assert_eq!(
            size_of::<rkyv::Archived<PeerIdBytes>>(),
            size_of::<rkyv::Archived<Vec<u8>>>()
        );
        assert_eq!(
            size_of::<rkyv::Archived<BattleNumber>>(),
            size_of::<rkyv::Archived<u32>>()
        );
        assert_eq!(
            size_of::<rkyv::Archived<RoundNumber>>(),
            size_of::<rkyv::Archived<u32>>()
        );
        assert_eq!(
            size_of::<rkyv::Archived<TurnNumber>>(),
            size_of::<rkyv::Archived<u32>>()
        );
        assert_eq!(
            size_of::<rkyv::Archived<OutboundSeq>>(),
            size_of::<rkyv::Archived<u64>>()
        );
    }

    #[test]
    fn player_id_other_flips_between_peers() {
        assert_eq!(PlayerId::ZERO.other(), PlayerId::ONE);
        assert_eq!(PlayerId::ONE.other(), PlayerId::ZERO);
    }
}
