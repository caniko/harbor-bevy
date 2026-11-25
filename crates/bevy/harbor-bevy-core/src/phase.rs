use std::{collections::HashMap, hash::Hash, time::Duration};

use bevy::prelude::*;

use crate::Resettable;

/// One-shot timer for a phase.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct PhaseTimer {
    timer: Timer,
}

impl PhaseTimer {
    pub fn new(duration: Duration) -> Self {
        Self {
            timer: Timer::new(duration, TimerMode::Once),
        }
    }

    pub fn tick(&mut self, delta: Duration) {
        self.timer.tick(delta);
    }

    pub fn is_finished(&self) -> bool {
        self.timer.is_finished()
    }

    pub fn elapsed_secs(&self) -> f32 {
        self.timer.elapsed_secs()
    }

    pub fn remaining_secs(&self) -> f32 {
        self.timer.remaining_secs()
    }

    /// Restart the timer without changing its duration.
    pub fn reset(&mut self) {
        self.timer.reset();
    }
}

impl Default for PhaseTimer {
    fn default() -> Self {
        Self::new(Duration::ZERO)
    }
}

pub trait ActionReady {
    /// Whether this state can accept a new action.
    fn is_action_ready(&self) -> bool;
}

pub trait PhaseReadiness: ActionReady + Resettable {
    fn is_complete(&self) -> bool;
    fn mark_complete(&mut self);
}

/// Implement phase readiness for boolean tuple structs where `false` means
/// action-ready and `true` means complete.
#[macro_export]
macro_rules! impl_bool_phase_readiness {
    ($($type:ty),+ $(,)?) => {$(
        impl $crate::phase::ActionReady for $type {
            fn is_action_ready(&self) -> bool { !self.0 }
        }
        impl $crate::phase::PhaseReadiness for $type {
            fn is_complete(&self) -> bool { self.0 }
            fn mark_complete(&mut self) { self.0 = true; }
        }
        impl $crate::traits::Resettable for $type {}
    )+};
}

/// Tracks readiness for explicitly registered participants.
///
/// Bevy resources are singleton components: use one tracker per `Id` type in a world.
#[derive(Resource, Debug, Clone)]
pub struct PhaseReadinessTracker<Id: Eq + Hash + Clone + Send + Sync + 'static> {
    participants: HashMap<Id, bool>,
}

impl<Id: Eq + Hash + Clone + Send + Sync + 'static> Default for PhaseReadinessTracker<Id> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Id: Eq + Hash + Clone + Send + Sync + 'static> PhaseReadinessTracker<Id> {
    pub fn new() -> Self {
        Self {
            participants: HashMap::new(),
        }
    }

    /// Register a participant as not ready. Does not change an existing entry.
    pub fn register(&mut self, id: Id) -> bool {
        if let std::collections::hash_map::Entry::Vacant(entry) = self.participants.entry(id) {
            entry.insert(false);
            true
        } else {
            false
        }
    }

    /// Mark a registered participant as ready; return true only on a transition.
    pub fn mark_ready(&mut self, id: &Id) -> bool {
        self.participants
            .get_mut(id)
            .is_some_and(|ready| !std::mem::replace(ready, true))
    }

    pub fn is_ready(&self, id: &Id) -> bool {
        self.participants.get(id).copied().unwrap_or(false)
    }

    /// An empty roster is not ready.
    pub fn all_ready(&self) -> bool {
        !self.participants.is_empty() && self.participants.values().all(|&ready| ready)
    }

    pub fn ready_count(&self) -> usize {
        self.participants.values().filter(|&&ready| ready).count()
    }

    pub fn total_count(&self) -> usize {
        self.participants.len()
    }

    /// Clear readiness without discarding the roster.
    pub fn reset(&mut self) {
        for ready in self.participants.values_mut() {
            *ready = false;
        }
    }

    /// Iterate through registered IDs in arbitrary order.
    pub fn participants(&self) -> impl ExactSizeIterator<Item = &Id> {
        self.participants.keys()
    }
}
