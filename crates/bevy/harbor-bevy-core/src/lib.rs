//! Shared application utilities for Bevy.

pub mod change_notifier;
pub mod cue_diff;
pub mod fatal;
pub mod file_store;
pub mod harness;
pub mod identity;
pub mod interpolation;
pub mod observation_buffer;
pub mod observer_ordering;
pub mod phase;
pub mod traits;

pub use change_notifier::{AppChangeNotifierExt, ComponentChanged, ResourceChanged};
pub use fatal::{FatalCode, GracefulShutdownPlugin, GracefulShutdownRequest, LastFatalExit};
pub use file_store::{DurableFileStore, DurableStoreError, FileStore, FileStoreError};
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

/// Both paths must be on the same filesystem. Sync the data, and on Unix sync its directory entry.
fn write_then_rename(
    temporary: &std::path::Path,
    destination: &std::path::Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    #[cfg(unix)]
    let parent = open_sync_directory(parent_dir(destination))?;
    write_synced_then_rename(temporary, destination, bytes)?;
    #[cfg(unix)]
    parent.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn open_sync_directory(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let parent = std::fs::File::open(path)?;
    if !parent.metadata()?.is_dir() {
        return Err(std::io::ErrorKind::NotADirectory.into());
    }
    Ok(parent)
}

#[cfg(not(unix))]
fn open_sync_directory(_: &std::path::Path) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "durable file storage requires Unix directory sync",
    ))
}

fn parent_dir(path: &std::path::Path) -> &std::path::Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."))
}

fn write_synced_then_rename(
    temporary: &std::path::Path,
    destination: &std::path::Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    use std::{fs, io::Write};

    let mut file = fs::File::create_new(temporary)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = result.and_then(|()| fs::rename(temporary, destination));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
