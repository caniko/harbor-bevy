//! Track presentation cues and entity lifetimes without depending on playback.

use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    time::Duration,
};

use bevy::prelude::{Commands, Entity, Resource};

/// Keys that started, persisted, or ended in an update, sorted within each group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CueDiff<K> {
    pub started: Vec<K>,
    pub persisted: Vec<K>,
    pub ended: Vec<K>,
}

#[derive(Clone, Debug, Resource)]
pub struct ActiveCueSet<K>(BTreeSet<K>);

impl<K> Default for ActiveCueSet<K> {
    fn default() -> Self {
        Self(BTreeSet::new())
    }
}

impl<K: Clone + Ord> ActiveCueSet<K> {
    /// Replace the active keys and report the changes. Duplicate input keys
    /// contribute only once.
    pub fn update(&mut self, active: impl IntoIterator<Item = K>) -> CueDiff<K> {
        let next: BTreeSet<_> = active.into_iter().collect();
        let diff = CueDiff {
            started: next.difference(&self.0).cloned().collect(),
            persisted: self.0.intersection(&next).cloned().collect(),
            ended: self.0.difference(&next).cloned().collect(),
        };
        self.0 = next;
        diff
    }
}

/// Despawns tracked entities at their deadlines, even in apps with no playback
/// plugins. An entity that playback already despawned is harmless.
#[derive(Debug, Resource)]
pub struct TimedEntityReaper<K>(BTreeMap<K, (Entity, Duration)>);

impl<K> Default for TimedEntityReaper<K> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<K: Ord> TimedEntityReaper<K> {
    pub fn contains(&self, key: &K) -> bool {
        self.0.contains_key(key)
    }

    /// Track a cue until an absolute deadline in the caller's elapsed-time
    /// clock. Returns `false` without replacing a previously tracked key.
    pub fn track(&mut self, key: K, entity: Entity, deadline: Duration) -> bool {
        if let Entry::Vacant(entry) = self.0.entry(key) {
            entry.insert((entity, deadline));
            return true;
        }
        false
    }

    /// Remove expired entries and queue their despawns. Returns the number of
    /// entries removed, including those whose entities had already despawned.
    pub fn reap_expired(&mut self, now: Duration, commands: &mut Commands) -> usize {
        let previous_len = self.0.len();
        // ponytail: scan once per call; index by deadline if large cue sets need it.
        self.0.retain(|_, (entity, deadline)| {
            if now < *deadline {
                return true;
            }
            commands.entity(*entity).try_despawn();
            false
        });
        previous_len - self.0.len()
    }
}
