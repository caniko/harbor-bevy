//! Observer-driven updates for applications with responsive UI roots.

use bevy::prelude::*;

use crate::viewport::{CurrentViewport, LayoutVersion, ViewportMetrics, ViewportSystems};

/// Mark responsive roots so application observers can query them selectively.
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct LayoutReactive;

/// A new settled viewport snapshot. Observe it to update or rebuild UI roots.
#[derive(Event, Debug, Clone, Copy)]
pub struct RelayoutTriggered {
    pub version: LayoutVersion,
    pub viewport: ViewportMetrics,
}

fn emit_relayout(
    version: Res<LayoutVersion>,
    viewport: Res<CurrentViewport>,
    mut emitted: Local<LayoutVersion>,
    mut commands: Commands,
) {
    if *emitted != *version {
        *emitted = *version;
        commands.trigger(RelayoutTriggered {
            version: *version,
            viewport: viewport.0,
        });
    }
}

/// Emit a global observer event after the viewport publishes a new version.
pub struct LayoutReactivePlugin;

impl Plugin for LayoutReactivePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LayoutVersion>()
            .init_resource::<CurrentViewport>()
            .add_systems(Update, emit_relayout.after(ViewportSystems));
    }
}
