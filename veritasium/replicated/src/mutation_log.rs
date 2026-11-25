//! Plain mutation log used by the replicated transaction chain.
//!
//! # Inline source-of-truth policy (Phase 05 / Option A)
//!
//! This module is an inlined, self-contained copy of the prototype
//! `bevy_ecs::mutation_log` primitive. It is kept here — rather than consumed
//! from the vendored Bevy fork — so that `bevy_replicated` compiles against
//! stock Bevy: the primitive uses only `std` + `blake3`, with no Bevy ECS
//! types. The narrowed deterministic-mutation RFC does **not** propose
//! `MutationLog` for upstream Bevy, so this inlined copy is the permanent home
//! of the primitive for this crate.
//!
//! The copy is deliberately kept faithful to the original prototype API —
//! including the parts `bevy_replicated` does not currently exercise (the
//! `Reflect` entry path, `append`, `entries`) — so that, should an upstream
//! equivalent ever appear, it can be swapped in cleanly. That unused surface is
//! retained on purpose; hence the module-level `dead_code` allow.
//!
//! See `docs/src/architecture/bevy-replicated-vendor-coupling.md`.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::marker::PhantomData;

/// A marker trait for the component set recorded by a [`MutationLog`].
pub trait MutationLogSet: Send + Sync + 'static {
    /// Stable domain tag mixed into the genesis hash.
    const DOMAIN_TAG: &'static [u8];

    /// Stable domain version mixed into the genesis hash.
    const DOMAIN_VERSION: u32 = 1;

    /// Stable entry tag mixed into every appended entry.
    const ENTRY_TAG: &'static [u8] = b"entry";
}

/// A single mutation-log entry for a [`MutationLogSet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEntry<S: MutationLogSet> {
    /// Reflect-backed component bytes produced by the default prototype path.
    Reflect {
        /// A stable component label for diagnostics and log readers.
        component: &'static str,
        /// Serialized post-mutation component state.
        bytes: Vec<u8>,
        /// Marker tying this entry to its set.
        marker: PhantomData<fn() -> S>,
    },
    /// Consumer-supplied canonical bytes.
    ///
    /// This lets downstream crates preserve an existing wire format while still
    /// routing their rolling hash and history through the primitive.
    CanonicalBytes {
        /// Stable bytes that define this mutation for hashing and replay.
        bytes: Vec<u8>,
        /// Marker tying this entry to its set.
        marker: PhantomData<fn() -> S>,
    },
}

impl<S: MutationLogSet> LogEntry<S> {
    /// Creates a reflect-backed log entry.
    pub fn reflect(component: &'static str, bytes: impl Into<Vec<u8>>) -> Self {
        Self::Reflect {
            component,
            bytes: bytes.into(),
            marker: PhantomData,
        }
    }

    /// Creates a consumer-supplied canonical byte entry.
    pub fn canonical_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self::CanonicalBytes {
            bytes: bytes.into(),
            marker: PhantomData,
        }
    }

    /// Returns the stable bytes carried by this entry.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Reflect { bytes, .. } | Self::CanonicalBytes { bytes, .. } => bytes,
        }
    }
}

/// Rolling mutation log for a declared component set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationLog<S: MutationLogSet> {
    current_hash: [u8; 32],
    history: VecDeque<[u8; 32]>,
    history_capacity: usize,
    history_floor: u64,
    entries: Vec<LogEntry<S>>,
}

impl<S: MutationLogSet> MutationLog<S> {
    /// Creates a log with the default blake3 genesis hash for `S`.
    pub fn new() -> Self {
        Self::with_genesis_hash(blake3_digest(&[
            S::DOMAIN_TAG,
            &S::DOMAIN_VERSION.to_le_bytes(),
            b"blake3",
        ]))
    }

    /// Creates a log with a caller-provided genesis hash.
    ///
    /// This is intended for downstream compatibility wrappers with existing
    /// domain tags or archived-byte policies.
    pub fn with_genesis_hash(current_hash: [u8; 32]) -> Self {
        Self::with_genesis_hash_and_capacity(current_hash, crate::ChainHistoryCapacity::default().0)
    }

    /// Creates a log with a caller-provided genesis hash and history capacity.
    pub fn with_genesis_hash_and_capacity(current_hash: [u8; 32], history_capacity: usize) -> Self {
        let history_capacity = history_capacity.max(1);
        let mut history = VecDeque::with_capacity(history_capacity.saturating_add(1));
        history.push_back(current_hash);
        Self {
            current_hash,
            history,
            history_capacity,
            history_floor: 0,
            entries: Vec::new(),
        }
    }

    /// Returns the rolling hash after the most recent entry.
    pub fn current_hash(&self) -> [u8; 32] {
        self.current_hash
    }

    /// Returns all entries in append order.
    pub fn entries(&self) -> &[LogEntry<S>] {
        &self.entries
    }

    /// Returns the hash at `offset`, where `0` is the genesis hash.
    pub fn hash_at(&self, offset: usize) -> Option<[u8; 32]> {
        let count = u64::try_from(offset).ok()?;
        self.hash_at_count(count)
    }

    /// Returns the hash at `count`, where `0` is the genesis hash.
    pub fn hash_at_count(&self, count: u64) -> Option<[u8; 32]> {
        if count < self.history_floor {
            return None;
        }
        let index = usize::try_from(count - self.history_floor).ok()?;
        self.history.get(index).copied()
    }

    /// Returns the oldest applied count still retained in the ring buffer.
    pub fn oldest_available_count(&self) -> u64 {
        self.history_floor
    }

    /// Appends an entry and folds it with the prototype blake3 algorithm.
    pub fn append(&mut self, entry: LogEntry<S>) {
        let next_hash = blake3_digest(&[
            &self.current_hash,
            S::ENTRY_TAG,
            &(entry.bytes().len() as u64).to_le_bytes(),
            entry.bytes(),
        ]);
        self.append_with_hash(entry, next_hash);
    }

    /// Appends an entry with a caller-computed rolling hash.
    pub fn append_with_hash(&mut self, entry: LogEntry<S>, next_hash: [u8; 32]) {
        self.current_hash = next_hash;
        self.entries.push(entry);
        self.push_history(next_hash);
    }

    fn push_history(&mut self, digest: [u8; 32]) {
        if self.history.len() == self.history_capacity.saturating_add(1) {
            self.history.pop_front();
            self.history_floor = self.history_floor.saturating_add(1);
        }
        self.history.push_back(digest);
    }
}

impl<S: MutationLogSet> Default for MutationLog<S> {
    fn default() -> Self {
        Self::new()
    }
}

fn blake3_digest(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestSet;

    impl MutationLogSet for TestSet {
        const DOMAIN_TAG: &'static [u8] = b"bevy_ecs.mutation_log.test.v1";
    }

    #[test]
    fn genesis_and_append_are_deterministic() {
        let mut left = MutationLog::<TestSet>::new();
        let mut right = MutationLog::<TestSet>::new();

        assert_eq!(left.current_hash(), right.current_hash());
        assert_eq!(left.hash_at(0), Some(left.current_hash()));

        left.append(LogEntry::canonical_bytes([1, 2, 3]));
        right.append(LogEntry::canonical_bytes([1, 2, 3]));

        assert_eq!(left.current_hash(), right.current_hash());
        assert_eq!(left.hash_at(1), Some(left.current_hash()));
        assert_eq!(left.entries().len(), 1);
    }
}
