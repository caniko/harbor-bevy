//! Blend fixed-step snapshots for presentation without changing simulation state.

use bevy::prelude::{FloatExt, Quat, Resource, SystemSet, Transform, Vec2, Vec3};

/// A pose that can be blended with another snapshot of the same type.
pub trait Interpolatable {
    /// Blend from `self` at `0.0` toward `next` at `1.0`.
    fn interpolate(&self, next: &Self, alpha: f32) -> Self;
}

impl Interpolatable for f32 {
    fn interpolate(&self, next: &Self, alpha: f32) -> Self {
        self.lerp(*next, alpha)
    }
}

impl Interpolatable for Vec2 {
    fn interpolate(&self, next: &Self, alpha: f32) -> Self {
        self.lerp(*next, alpha)
    }
}

impl Interpolatable for Vec3 {
    fn interpolate(&self, next: &Self, alpha: f32) -> Self {
        self.lerp(*next, alpha)
    }
}

impl Interpolatable for Quat {
    fn interpolate(&self, next: &Self, alpha: f32) -> Self {
        self.slerp(*next, alpha)
    }
}

impl Interpolatable for Transform {
    fn interpolate(&self, next: &Self, alpha: f32) -> Self {
        Self {
            translation: self.translation.interpolate(&next.translation, alpha),
            rotation: self.rotation.interpolate(&next.rotation, alpha),
            scale: self.scale.interpolate(&next.scale, alpha),
        }
    }
}

/// The two latest committed pose snapshots. Advance in the fixed schedule and
/// sample in the presentation schedule with the fixed-time overstep fraction.
#[derive(Clone, Debug, Resource)]
pub struct PoseHistory<T> {
    previous: T,
    current: T,
}

impl<T: Clone> PoseHistory<T> {
    /// Seed both snapshots with the same pose to avoid interpolating from a placeholder.
    pub fn new(pose: T) -> Self {
        Self {
            previous: pose.clone(),
            current: pose,
        }
    }

    /// Seed both snapshots after a discontinuous change.
    pub fn reset(&mut self, pose: T) {
        *self = Self::new(pose);
    }
}

impl<T: Clone + Default> Default for PoseHistory<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> PoseHistory<T> {
    /// Advance to a newly committed pose.
    pub fn push(&mut self, pose: T) {
        self.previous = std::mem::replace(&mut self.current, pose);
    }

    pub fn previous(&self) -> &T {
        &self.previous
    }

    pub fn current(&self) -> &T {
        &self.current
    }
}

impl<T: Interpolatable> PoseHistory<T> {
    pub fn interpolated(&self, alpha: f32) -> T {
        self.previous.interpolate(&self.current, alpha)
    }
}

/// Put presentation interpolation systems in this set to let consumers order
/// them relative to other systems (for example, before transform propagation).
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InterpolatePresentationSet;
