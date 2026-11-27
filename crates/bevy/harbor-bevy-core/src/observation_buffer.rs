//! Hand committed tick observations to presentation without overwriting them.

use std::collections::{BTreeMap, btree_map::Entry};

use bevy::prelude::Resource;

/// A bounded buffer keyed by `(generation, tick)`. The caller owns retention:
/// nothing is evicted automatically when the buffer fills.
#[derive(Debug, Resource)]
pub struct CommittedTickObservationBuffer<T> {
    observations: BTreeMap<(u64, u32), T>,
    capacity: usize,
}

impl<T> CommittedTickObservationBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            observations: BTreeMap::new(),
            capacity,
        }
    }

    /// Store an observation. Returns `false` if the key already exists or the
    /// buffer is full, leaving all existing observations intact.
    pub fn publish(&mut self, generation: u64, tick: u32, observation: T) -> bool {
        if self.len() == self.capacity {
            return false;
        }
        if let Entry::Vacant(entry) = self.observations.entry((generation, tick)) {
            entry.insert(observation);
            return true;
        }
        false
    }

    pub fn get(&self, generation: u64, tick: u32) -> Option<&T> {
        self.observations.get(&(generation, tick))
    }

    /// Drop observations with keys strictly below `(generation, tick)`.
    pub fn retire_before(&mut self, generation: u64, tick: u32) {
        self.observations = self.observations.split_off(&(generation, tick));
    }

    pub fn clear(&mut self) {
        self.observations.clear();
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.observations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }
}
