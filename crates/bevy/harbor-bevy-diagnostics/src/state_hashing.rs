//! Deterministic hashes of caller-defined state.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

#[derive(Default)]
pub struct StateHasher(blake3::Hasher);

impl StateHasher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes in canonical order. Callers must frame variable-length fields.
    pub fn update(&mut self, bytes: impl AsRef<[u8]>) {
        self.0.update(bytes.as_ref());
    }

    pub fn finalize(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

/// Encode relevant state without using runtime IDs, platform-sized integers, or unordered maps.
pub trait HashedState {
    fn hash_state(&self, hasher: &mut StateHasher);
}

pub fn hash_state_value<T: HashedState + ?Sized>(value: &T) -> [u8; 32] {
    let mut hasher = StateHasher::new();
    value.hash_state(&mut hasher);
    hasher.finalize()
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bevy", derive(bevy::prelude::Resource))]
pub struct StateHashConfig {
    /// Number of schedule runs between hashes. Zero disables hashing.
    pub interval_ticks: u64,
    /// Number of retained hashes. Zero means unbounded.
    pub max_entries: usize,
    pub instance_id: String,
}

impl Default for StateHashConfig {
    fn default() -> Self {
        Self {
            interval_ticks: 60,
            max_entries: 512,
            instance_id: String::new(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HashEntry {
    pub tick: u64,
    pub hash: [u8; 32],
    pub component_count: usize,
}

impl fmt::Debug for HashEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tick={} hash={}.. components={}",
            self.tick,
            hex(&self.hash[..4]),
            self.component_count
        )
    }
}

#[derive(Default)]
#[cfg_attr(feature = "bevy", derive(bevy::prelude::Resource))]
pub struct StateHashLog {
    entries: Vec<HashEntry>,
}

impl StateHashLog {
    pub fn entries(&self) -> &[HashEntry] {
        &self.entries
    }

    pub fn push_entry(&mut self, entry: HashEntry, max_entries: usize) {
        if max_entries != 0 {
            let evict = self.entries.len().saturating_sub(max_entries - 1);
            if evict != 0 {
                drop(self.entries.drain(..evict));
            }
        }
        self.entries.push(entry);
    }

    /// Report differing hashes only for ticks present in both logs.
    pub fn diff(left: &Self, right: &Self) -> Vec<u64> {
        let indexed = |log: &Self| -> BTreeMap<_, _> {
            log.entries
                .iter()
                .map(|entry| (entry.tick, entry.hash))
                .collect()
        };
        let right = indexed(right);
        indexed(left)
            .into_iter()
            .filter_map(|(tick, hash)| {
                right
                    .get(&tick)
                    .is_some_and(|other| *other != hash)
                    .then_some(tick)
            })
            .collect()
    }

    pub fn summary(&self) -> String {
        let mut out = format!("StateHashLog: {} entries\n", self.entries.len());
        for entry in &self.entries[self.entries.len().saturating_sub(10)..] {
            let _ = writeln!(out, "  {entry:?}");
        }
        out
    }

    pub fn to_json(&self) -> String {
        let entries: Vec<_> = self
            .entries
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "tick": entry.tick,
                    "hash": hex(&entry.hash),
                    "component_count": entry.component_count,
                })
            })
            .collect();
        serde_json::json!({ "entries": entries }).to_string()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// A caller-maintained digest. Refresh it when the relevant state changes.
///
/// The aggregate is a multiset: if entity identity matters, include a stable
/// application ID in the value's [`HashedState`] implementation.
#[cfg(feature = "bevy")]
#[derive(bevy::prelude::Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HashTarget([u8; 32]);

#[cfg(feature = "bevy")]
impl HashTarget {
    pub fn new<T: HashedState + ?Sized>(value: &T) -> Self {
        Self(hash_state_value(value))
    }

    pub fn update<T: HashedState + ?Sized>(&mut self, value: &T) {
        *self = Self::new(value);
    }
}

/// Record a hash at configured intervals, independent of entity allocation and query order.
#[cfg(feature = "bevy")]
pub fn periodic_state_hash(
    config: bevy::prelude::Res<StateHashConfig>,
    mut log: bevy::prelude::ResMut<StateHashLog>,
    targets: bevy::prelude::Query<&HashTarget>,
    mut tick: bevy::prelude::Local<u64>,
) {
    *tick = tick.saturating_add(1);
    if config.interval_ticks == 0 || !tick.is_multiple_of(config.interval_ticks) {
        return;
    }
    let mut digests: Vec<_> = targets.iter().map(|target| target.0).collect();
    digests.sort_unstable();
    let mut hasher = StateHasher::new();
    for digest in &digests {
        hasher.update(digest);
    }
    log.push_entry(
        HashEntry {
            tick: *tick,
            hash: hasher.finalize(),
            component_count: digests.len(),
        },
        config.max_entries,
    );
}
