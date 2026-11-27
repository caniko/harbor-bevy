//! Hit-test a displayed snapshot, then keep routing the gesture to its owner.

use std::collections::VecDeque;

use bevy::prelude::Resource;

/// Identifies one pointer; adapters must use distinct IDs for concurrent pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct PointerId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum PointerPhase {
    Press,
    Move,
    Release,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct PointerSample {
    pub id: PointerId,
    pub phase: PointerPhase,
    /// Logical `[x, y]`, in the same space as the displayed bounds.
    pub position: [f32; 2],
}

/// Top-left-inclusive, bottom-right-exclusive logical bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct LogicalRect {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl LogicalRect {
    /// Test a point. Invalid or empty bounds never hit.
    pub fn contains(self, point: [f32; 2]) -> bool {
        (0..2).all(|i| {
            self.min[i].is_finite()
                && self.max[i].is_finite()
                && self.min[i] <= point[i]
                && point[i] < self.max[i]
        })
    }

    /// Width and height; invalid or inverted axes have zero extent.
    pub fn size(self) -> [f32; 2] {
        std::array::from_fn(|i| {
            let span = self.max[i] - self.min[i];
            if span.is_finite() { span.max(0.0) } else { 0.0 }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct DisplayedHitTarget<T> {
    pub target: T,
    pub bounds: LogicalRect,
    /// Higher values win. At equal z, the last displayed entry wins.
    pub z_order: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct PointerRouteDecision<T> {
    pub sample: PointerSample,
    /// Gesture owner, even if the pointer or target is no longer in the displayed bounds.
    pub target: Option<T>,
}

/// Active pointer captures and optional diagnostics. Persist individual
/// [`PointerRouteDecision`]s rather than the live router state.
#[derive(Debug, Resource)]
pub struct PointerRouter<T> {
    // ponytail: linear lookup over active pointers; use a map if pointer counts grow.
    captures: Vec<(PointerId, T)>,
    trace_capacity: usize,
    trace: VecDeque<PointerRouteDecision<T>>,
}

impl<T> Default for PointerRouter<T> {
    fn default() -> Self {
        Self {
            captures: Vec::new(),
            trace_capacity: 0,
            trace: VecDeque::new(),
        }
    }
}

impl<T> PointerRouter<T> {
    /// Keep up to `capacity` decisions. Zero disables tracing and clears history.
    pub fn set_trace_capacity(&mut self, capacity: usize) {
        self.trace_capacity = capacity;
        self.trace
            .drain(..self.trace.len().saturating_sub(capacity));
    }

    /// Retained decisions from oldest to newest.
    pub fn trace(&self) -> &VecDeque<PointerRouteDecision<T>> {
        &self.trace
    }
}

impl<T: Clone> PointerRouter<T> {
    /// Capture the topmost hit on press. A repeated press keeps existing
    /// capture; a release or cancellation ends it. Uncaptured moves and
    /// releases never hit-test. Adapters should send `Cancel` on pointer loss.
    pub fn route(
        &mut self,
        sample: PointerSample,
        displayed: &[DisplayedHitTarget<T>],
    ) -> PointerRouteDecision<T> {
        let captured = self.captures.iter().position(|entry| entry.0 == sample.id);
        let target = match (sample.phase, captured) {
            (PointerPhase::Release | PointerPhase::Cancel, Some(i)) => {
                Some(self.captures.swap_remove(i).1)
            }
            (_, Some(i)) => Some(self.captures[i].1.clone()),
            (PointerPhase::Press, None) => displayed
                .iter()
                .filter(|entry| entry.bounds.contains(sample.position))
                .max_by_key(|entry| entry.z_order)
                .map(|entry| {
                    self.captures.push((sample.id, entry.target.clone()));
                    entry.target.clone()
                }),
            (_, None) => None,
        };
        let decision = PointerRouteDecision { sample, target };
        if self.trace_capacity != 0 {
            if self.trace.len() == self.trace_capacity {
                self.trace.pop_front();
            }
            self.trace.push_back(decision.clone());
        }
        decision
    }
}
