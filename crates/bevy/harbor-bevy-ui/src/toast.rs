//! Lifetime management for temporary UI entities.

use std::time::Duration;

use bevy::prelude::*;

/// Despawn this entity and its descendants when the timer expires.
#[derive(Component, Debug)]
pub struct ToastTimer(pub Timer);

impl ToastTimer {
    /// Invalid durations expire on the next tick.
    pub fn from_secs(secs: f32) -> Self {
        Self(Timer::new(
            Duration::try_from_secs_f32(secs).unwrap_or_default(),
            TimerMode::Once,
        ))
    }
}

/// Tick toasts with application time, so they respect app pause and time scaling.
pub fn tick_toast_timers(
    time: Res<Time>,
    mut toasts: Query<(Entity, &mut ToastTimer)>,
    mut commands: Commands,
) {
    for (entity, mut timer) in &mut toasts {
        if timer.0.tick(time.delta()).is_finished() {
            commands.entity(entity).try_despawn();
        }
    }
}

/// Installs toast cleanup. Use with Bevy's `TimePlugin`.
pub struct ToastPlugin;

impl Plugin for ToastPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, tick_toast_timers);
    }
}
