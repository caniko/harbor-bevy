//! Shared application utilities for Bevy.

pub mod change_notifier;
pub mod fatal;
pub mod file_store;
pub mod harness;
pub mod identity;
pub mod observer_ordering;
pub mod phase;
pub mod traits;

pub use change_notifier::{AppChangeNotifierExt, ComponentChanged, ResourceChanged};
pub use fatal::{FatalCode, GracefulShutdownPlugin, GracefulShutdownRequest, LastFatalExit};
pub use file_store::{FileStore, FileStoreError};
pub use identity::AppIdentity;
pub use observer_ordering::{AppObserverOrderingExt, ObserverNameExt, ObserverOrderingPlugin};
pub use phase::{ActionReady, PhaseReadiness, PhaseReadinessTracker, PhaseTimer};
pub use traits::Resettable;

/// Installs graceful shutdown and observer ordering unless already registered.
pub struct HarborCorePlugin;

impl bevy::app::Plugin for HarborCorePlugin {
    fn build(&self, app: &mut bevy::app::App) {
        if !app.is_plugin_added::<GracefulShutdownPlugin>() {
            app.add_plugins(GracefulShutdownPlugin);
        }
        if !app.is_plugin_added::<ObserverOrderingPlugin>() {
            app.add_plugins(ObserverOrderingPlugin);
        }
    }
}

#[cfg(test)]
#[test]
fn core_plugin_reuses_existing_shutdown_plugin() {
    use bevy::app::App;

    let mut app = App::new();
    app.add_plugins(GracefulShutdownPlugin)
        .add_plugins(HarborCorePlugin);
    assert!(app.is_plugin_added::<ObserverOrderingPlugin>());
}

/// Both paths must be on the same filesystem. Publish only a complete write.
fn write_then_rename(
    temporary: &std::path::Path,
    destination: &std::path::Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    use std::{fs, io::Write};

    let mut file = fs::File::create_new(temporary)?;
    let result = file.write_all(bytes);
    drop(file);
    let result = result.and_then(|()| fs::rename(temporary, destination));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
