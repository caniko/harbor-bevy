//! Central deduplication primitives for inbound replication traffic.
//!
//! Per-receiver bespoke `LastApplied*` / `LastReceived*` resources re-invent
//! dedup and break under dual delivery (typed actor message + broadcast
//! fallback). These two structures are the single mechanism receivers should
//! use:
//!
//! - [`SeenSet`] — map-backed, for unbounded partition spaces.
//! - [`KindSenderSeenSet`] — array-backed, for the bounded
//!   `(message kind, sender seat)` partition space of a session.
//!
//! Both record the highest key observed per partition and accept only
//! strictly-newer keys, so duplicates *and* stale out-of-order arrivals are
//! rejected by the same comparison.

use std::hash::Hash;
use std::marker::PhantomData;

use bevy::prelude::*;
use rustc_hash::FxHashMap;

/// Generic monotonic-key dedup set.
///
/// `K` is the key type that identifies a message instance. Each key has a
/// totally-ordered comparison via `Ord`; the set accepts only strictly
/// newer keys per `partition`. `partition` is whatever scopes the
/// monotonic counter — typically `(kind, sender)` or just `sender`.
///
/// The two main use shapes:
///
/// ```ignore
/// // Per-turn dedup keyed by (turn, player_id).
/// let mut seen: SeenSet<u8, (u32, u8)> = SeenSet::default();
/// if !seen.try_accept(kind_tag, (turn, player_id)) {
///     return;  // duplicate — drop
/// }
///
/// // Per-message-kind monotonic sequence.
/// let mut seen: SeenSet<(KindTag, [u8; 38]), u64> = SeenSet::default();
/// if !seen.try_accept((kind, sender_peer_id), sequence) { return; }
/// ```
#[derive(Resource, Debug)]
pub struct SeenSet<P, K>
where
    P: Eq + Hash + Clone,
    K: Ord + Copy,
{
    last_seen: FxHashMap<P, K>,
}

impl<P, K> Default for SeenSet<P, K>
where
    P: Eq + Hash + Clone,
    K: Ord + Copy,
{
    fn default() -> Self {
        Self {
            last_seen: FxHashMap::default(),
        }
    }
}

impl<P, K> SeenSet<P, K>
where
    P: Eq + Hash + Clone,
    K: Ord + Copy,
{
    /// Returns `true` if `key` is strictly greater than any previously seen
    /// key for `partition`, recording the new high-water mark. Returns
    /// `false` for duplicates and out-of-order/stale arrivals.
    pub fn try_accept(&mut self, partition: P, key: K) -> bool {
        match self.last_seen.get(&partition) {
            Some(prev) if *prev >= key => false,
            _ => {
                self.last_seen.insert(partition, key);
                true
            }
        }
    }

    /// Look up the latest accepted key for a partition (read-only).
    pub fn latest(&self, partition: &P) -> Option<K> {
        self.last_seen.get(partition).copied()
    }

    /// Drop all state for a partition (e.g. on session reset).
    pub fn forget(&mut self, partition: &P) {
        self.last_seen.remove(partition);
    }

    /// Wipe all partitions.
    pub fn clear(&mut self) {
        self.last_seen.clear();
    }

    /// Number of distinct partitions tracked.
    pub fn len(&self) -> usize {
        self.last_seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.last_seen.is_empty()
    }
}

/// Dense-index view of a message-kind enum, for array-backed dedup.
///
/// Implementors promise `kind_index()` is a bijection onto
/// `[0, Self::COUNT)`. Wire tags are a separate concern — this index is
/// in-memory layout only and may change between builds.
pub trait KindIndex: Copy {
    /// Number of kinds; the array dimension.
    const COUNT: usize;

    /// Dense `[0, Self::COUNT)` index for this kind.
    fn kind_index(self) -> usize;
}

/// Array-backed seen-set partitioned by `(kind, sender seat)`.
///
/// For bounded partition spaces (`Kind::COUNT × MAX_SENDERS` slots) this
/// eliminates the per-message hash + branch that [`SeenSet`] incurs. The
/// sentinel for "never seen" is `K::default()`, which is safe as long as
/// real keys start above it (sequence and turn counters start at 1).
///
/// Out-of-range sender indices are dropped as duplicates (defensive — a
/// wire payload claiming a seat `>= MAX_SENDERS` is malformed).
#[derive(Resource, Debug)]
pub struct KindSenderSeenSet<Kind, K, const MAX_SENDERS: usize>
where
    Kind: KindIndex + Send + Sync + 'static,
    K: Ord + Copy + Default + Send + Sync + 'static,
{
    // Row-major: `slots[kind_index * MAX_SENDERS + sender]`.
    slots: Box<[K]>,
    _kind: PhantomData<fn(Kind)>,
}

impl<Kind, K, const MAX_SENDERS: usize> Default for KindSenderSeenSet<Kind, K, MAX_SENDERS>
where
    Kind: KindIndex + Send + Sync + 'static,
    K: Ord + Copy + Default + Send + Sync + 'static,
{
    fn default() -> Self {
        Self {
            slots: vec![K::default(); Kind::COUNT * MAX_SENDERS].into_boxed_slice(),
            _kind: PhantomData,
        }
    }
}

impl<Kind, K, const MAX_SENDERS: usize> KindSenderSeenSet<Kind, K, MAX_SENDERS>
where
    Kind: KindIndex + Send + Sync + 'static,
    K: Ord + Copy + Default + Send + Sync + 'static,
{
    #[inline]
    fn slot_index(kind: Kind, sender: usize) -> usize {
        debug_assert!(
            kind.kind_index() < Kind::COUNT,
            "KindIndex bijection broken"
        );
        kind.kind_index() * MAX_SENDERS + sender
    }

    /// Returns `true` if `key` is strictly greater than any previously seen
    /// key for `(kind, sender)`, recording the new high-water mark.
    pub fn try_accept(&mut self, kind: Kind, sender: usize, key: K) -> bool {
        if sender >= MAX_SENDERS {
            return false;
        }
        let slot = &mut self.slots[Self::slot_index(kind, sender)];
        if *slot >= key {
            false
        } else {
            *slot = key;
            true
        }
    }

    /// Latest accepted key. Returns `None` if no message of this partition
    /// has ever been accepted (slot still equals `K::default()`).
    pub fn latest(&self, kind: Kind, sender: usize) -> Option<K> {
        if sender >= MAX_SENDERS {
            return None;
        }
        let v = self.slots[Self::slot_index(kind, sender)];
        (v != K::default()).then_some(v)
    }

    /// Reset state for one partition.
    pub fn forget(&mut self, kind: Kind, sender: usize) {
        if sender >= MAX_SENDERS {
            return;
        }
        self.slots[Self::slot_index(kind, sender)] = K::default();
    }

    /// Wipe all partitions.
    pub fn clear(&mut self) {
        self.slots.fill(K::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_strictly_increasing_keys() {
        let mut set: SeenSet<u8, u32> = SeenSet::default();
        assert!(set.try_accept(0u8, 1));
        assert!(set.try_accept(0u8, 2));
        assert!(set.try_accept(0u8, 3));
    }

    #[test]
    fn rejects_duplicate_key() {
        let mut set: SeenSet<u8, u32> = SeenSet::default();
        assert!(set.try_accept(0u8, 5));
        assert!(!set.try_accept(0u8, 5));
    }

    #[test]
    fn rejects_stale_key() {
        let mut set: SeenSet<u8, u32> = SeenSet::default();
        assert!(set.try_accept(0u8, 10));
        assert!(!set.try_accept(0u8, 9));
        assert!(!set.try_accept(0u8, 1));
    }

    #[test]
    fn partitions_are_independent() {
        let mut set: SeenSet<u8, u32> = SeenSet::default();
        assert!(set.try_accept(0u8, 5));
        // Same key in a different partition is accepted.
        assert!(set.try_accept(1u8, 5));
        // And each partition has its own monotonic state.
        assert!(set.try_accept(1u8, 6));
        assert!(!set.try_accept(0u8, 5));
    }

    #[derive(Clone, Copy)]
    enum TwoKinds {
        A,
        B,
    }

    impl KindIndex for TwoKinds {
        const COUNT: usize = 2;
        fn kind_index(self) -> usize {
            match self {
                Self::A => 0,
                Self::B => 1,
            }
        }
    }

    #[test]
    fn kind_sender_set_tracks_partitions_independently() {
        let mut set: KindSenderSeenSet<TwoKinds, u32, 2> = KindSenderSeenSet::default();
        assert!(set.try_accept(TwoKinds::A, 0, 1));
        assert!(set.try_accept(TwoKinds::A, 1, 1));
        assert!(set.try_accept(TwoKinds::B, 0, 1));
        assert!(!set.try_accept(TwoKinds::A, 0, 1));
        assert!(set.try_accept(TwoKinds::A, 0, 2));
        assert_eq!(set.latest(TwoKinds::A, 0), Some(2));
        assert_eq!(set.latest(TwoKinds::B, 1), None);
    }

    #[test]
    fn kind_sender_set_rejects_out_of_range_sender() {
        let mut set: KindSenderSeenSet<TwoKinds, u32, 2> = KindSenderSeenSet::default();
        assert!(!set.try_accept(TwoKinds::A, 2, 1));
        assert_eq!(set.latest(TwoKinds::A, 2), None);
    }

    #[test]
    fn kind_sender_set_forget_resets_partition() {
        let mut set: KindSenderSeenSet<TwoKinds, u32, 2> = KindSenderSeenSet::default();
        set.try_accept(TwoKinds::A, 0, 5);
        set.forget(TwoKinds::A, 0);
        assert!(set.try_accept(TwoKinds::A, 0, 1));
    }
}
