//! Scoped synchronization barrier for deterministic mutation pipelines.
//!
//! # Inline source-of-truth policy (Phase 05 / Option A)
//!
//! This module is an inlined, self-contained copy of the prototype
//! `bevy_ecs::sync_barrier` primitive. It is kept here — rather than consumed
//! from the vendored Bevy fork — so that `bevy_replicated` compiles against
//! stock Bevy: it uses only the public [`World::flush`]. The narrowed
//! deterministic-mutation RFC does **not** propose `SyncBarrier` for upstream
//! Bevy, so this inlined copy is the permanent home of the primitive for this
//! crate.
//!
//! See `docs/src/architecture/bevy-replicated-vendor-coupling.md`.

use std::marker::PhantomData;

use bevy::ecs::world::World;

/// Flushes pending writes for a declared component set.
///
/// The current implementation uses [`World::flush`] globally. The public
/// contract is intentionally phrased in terms of `S`: after the barrier runs,
/// pending writes to components in `S` are settled. A future implementation can
/// replace the global flush with per-set dispatch without changing downstream
/// schedules.
pub struct SyncBarrier<S> {
    marker: PhantomData<fn() -> S>,
}

impl<S> SyncBarrier<S> {
    /// Exclusive system suitable for schedule insertion.
    pub fn system(world: &mut World) {
        world.flush();
    }
}

impl<S> Default for SyncBarrier<S> {
    fn default() -> Self {
        Self {
            marker: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::schedule::IntoScheduleConfigs;
    use bevy::prelude::*;

    use super::*;

    #[derive(Component)]
    struct Marker;

    struct TestSet;

    #[test]
    fn barrier_flushes_deferred_commands() {
        fn spawn_marker(mut commands: Commands) {
            commands.spawn(Marker);
        }

        let mut world = World::new();
        let mut schedule = Schedule::default();
        schedule.add_systems((spawn_marker, SyncBarrier::<TestSet>::system).chain());
        schedule.run(&mut world);

        assert_eq!(world.query::<&Marker>().iter(&world).count(), 1);
    }
}
