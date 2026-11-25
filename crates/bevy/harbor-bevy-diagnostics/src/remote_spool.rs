//! Local storage for diagnostics queued by the caller. No upload is performed.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// The dedicated spool directory below the caller's application data directory.
pub fn spool_dir(base_dir: impl AsRef<Path>) -> PathBuf {
    base_dir.as_ref().join("remote-telemetry")
}

/// Delete only the dedicated spool. A missing spool counts as success.
pub fn clear_remote_spool(base_dir: impl AsRef<Path>) -> io::Result<()> {
    match fs::remove_dir_all(spool_dir(base_dir)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
