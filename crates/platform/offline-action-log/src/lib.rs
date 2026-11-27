//! Generic offline actions and their deterministic reconciliation order.
#![no_std]

extern crate alloc;

use core::fmt;

/// Stable identifier used for deduplication and ordering ties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct ActionId(pub [u8; 16]);

/// Identifies the device that authored an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct DeviceId(pub [u8; 16]);

/// A rejected negative Lamport timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegativeLamport(pub i64);

impl fmt::Display for NegativeLamport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lamport timestamp must be nonnegative (got {})", self.0)
    }
}

impl core::error::Error for NegativeLamport {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "i64", into = "i64")
)]
#[cfg_attr(feature = "rkyv", derive(rkyv::Archive, rkyv::Serialize))]
struct Lamport(i64);

impl Lamport {
    const fn new(value: i64) -> Result<Self, NegativeLamport> {
        if value < 0 {
            Err(NegativeLamport(value))
        } else {
            Ok(Self(value))
        }
    }
}

#[cfg(feature = "serde")]
impl From<Lamport> for i64 {
    fn from(value: Lamport) -> Self {
        value.0
    }
}

#[cfg(feature = "serde")]
impl TryFrom<i64> for Lamport {
    type Error = NegativeLamport;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[cfg(feature = "rkyv")]
impl<D> rkyv::Deserialize<Lamport, D> for ArchivedLamport
where
    D: rkyv::rancor::Fallible + ?Sized,
    D::Error: rkyv::rancor::Source,
{
    fn deserialize(&self, _: &mut D) -> Result<Lamport, D::Error> {
        Lamport::new(self.0.to_native()).map_err(<D::Error as rkyv::rancor::Source>::new)
    }
}

/// A client-authored action. `K` is the application's action payload.
///
/// When loading an rkyv archive, deserialize the entry to validate the Lamport
/// timestamp. Checked zero-copy access only verifies the archived layout.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(deserialize_bounds(__D::Error: rkyv::rancor::Source))
)]
pub struct ActionLogEntry<K> {
    pub action_id: ActionId,
    lamport: Lamport,
    pub device_id: DeviceId,
    pub client_wall_ms: i64,
    pub client_local_day: u32,
    pub monotonic_elapsed_ms: Option<u64>,
    pub depends_on: Option<ActionId>,
    pub context_checksum: Option<[u8; 32]>,
    pub kind: K,
}

impl<K> ActionLogEntry<K> {
    /// Creates an action without optional elapsed time, dependencies, or context.
    pub fn new(
        action_id: ActionId,
        lamport: i64,
        device_id: DeviceId,
        client_wall_ms: i64,
        client_local_day: u32,
        kind: K,
    ) -> Result<Self, NegativeLamport> {
        let lamport = Lamport::new(lamport)?;
        Ok(Self {
            action_id,
            lamport,
            device_id,
            client_wall_ms,
            client_local_day,
            monotonic_elapsed_ms: None,
            depends_on: None,
            context_checksum: None,
            kind,
        })
    }

    #[must_use]
    pub const fn lamport(&self) -> i64 {
        self.lamport.0
    }

    /// Orders by Lamport timestamp, then by action ID. Does not resolve dependencies.
    #[must_use]
    pub const fn reconciliation_key(&self) -> (i64, ActionId) {
        (self.lamport(), self.action_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum ActionAckStatus {
    Applied,
    AlreadyApplied,
    ClampedBySanityGuard,
    RejectedConflictingPayload,
    RejectedInsufficientBalance,
    RejectedInvalidPrecondition,
    RejectedRetryable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct ActionAck {
    pub action_id: ActionId,
    pub status: ActionAckStatus,
}

#[cfg(all(test, feature = "rkyv"))]
mod tests {
    use super::*;
    use rkyv::rancor::Error;

    #[test]
    fn rkyv_rejects_negative_lamport_on_deserialization() {
        let entry =
            ActionLogEntry::new(ActionId([0; 16]), 42, DeviceId([0; 16]), 0, 0, ()).unwrap();
        let mut bytes = rkyv::to_bytes::<Error>(&entry).unwrap();
        let offset = rkyv::api::root_position::<ArchivedActionLogEntry<()>>(bytes.len())
            + core::mem::offset_of!(ArchivedActionLogEntry<()>, lamport);
        bytes[offset..offset + 8].fill(0xff);
        assert!(rkyv::from_bytes::<ActionLogEntry<()>, Error>(&bytes).is_err());
    }
}
