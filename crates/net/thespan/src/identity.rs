//! Persistent libp2p peer identity.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use crate::ThespanConfig;

pub use libp2p::identity::Keypair;

pub const IDENTITY_DIR_ENV: &str = "THESPAN_IDENTITY_DIR";
pub const IDENTITY_FILE_NAME: &str = "identity.bin";

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("platform data directory is unavailable; set {env} to a persistent directory")]
    NoDataDir { env: String },
    #[error("application ID must be a single directory name: {0:?}")]
    InvalidAppId(String),
    #[error("identity I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to decode identity keypair from {path}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: libp2p::identity::DecodingError,
    },
    #[error("failed to encode identity keypair: {0}")]
    Encode(libp2p::identity::DecodingError),
}

/// Persistent identity location for the default [`ThespanConfig`].
pub fn identity_path() -> Result<PathBuf, IdentityError> {
    ThespanConfig::default().identity_path()
}

impl ThespanConfig {
    /// Resolve `identity.bin` inside the configured override directory, or under
    /// `<platform data directory>/<app_id>` when the override is unset.
    pub fn identity_path(&self) -> Result<PathBuf, IdentityError> {
        self.resolve_identity_path(|name| std::env::var_os(name), dirs::data_dir)
    }

    fn resolve_identity_path(
        &self,
        read_env: impl FnOnce(&str) -> Option<OsString>,
        data_dir: impl FnOnce() -> Option<PathBuf>,
    ) -> Result<PathBuf, IdentityError> {
        if let Some(dir) = read_env(&self.identity_dir_env) {
            return Ok(PathBuf::from(dir).join(IDENTITY_FILE_NAME));
        }
        if Path::new(&self.app_id).file_name() != Some(OsStr::new(&self.app_id)) {
            return Err(IdentityError::InvalidAppId(self.app_id.clone()));
        }
        let base = data_dir().ok_or_else(|| IdentityError::NoDataDir {
            env: self.identity_dir_env.clone(),
        })?;
        Ok(base.join(&self.app_id).join(IDENTITY_FILE_NAME))
    }

    /// Load or create the persistent identity for this application.
    pub fn load_or_create_keypair(&self) -> Result<Keypair, IdentityError> {
        load_or_create_keypair(&self.identity_path()?)
    }

    /// Remove this application's persistent identity; missing files are fine.
    pub fn reset_identity(&self) -> Result<PathBuf, IdentityError> {
        let path = self.identity_path()?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(path),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(path),
            Err(source) => Err(IdentityError::Io { path, source }),
        }
    }
}

/// Load an Ed25519 libp2p keypair from disk, or create it on first launch.
///
/// Writes are atomic (`*.tmp` + fsync + rename) so a partial write cannot
/// silently replace a user's persistent PeerId. On Unix the file is created
/// at mode 0600 and existing identity files are normalized to 0600.
pub fn load_or_create_keypair(path: &Path) -> Result<Keypair, IdentityError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let keypair = Keypair::from_protobuf_encoding(&bytes).map_err(|source| {
                IdentityError::Decode {
                    path: path.to_owned(),
                    source,
                }
            })?;
            restrict_identity_permissions(path)?;
            Ok(keypair)
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| IdentityError::Io {
                    path: parent.to_owned(),
                    source,
                })?;
            }
            let keypair = Keypair::generate_ed25519();
            let bytes = keypair
                .to_protobuf_encoding()
                .map_err(IdentityError::Encode)?;
            atomic_write(path, &bytes)?;
            Ok(keypair)
        }
        Err(source) => Err(IdentityError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

pub fn load_or_create_default_keypair() -> Result<Keypair, IdentityError> {
    ThespanConfig::default().load_or_create_keypair()
}

pub fn reset_identity() -> Result<PathBuf, IdentityError> {
    ThespanConfig::default().reset_identity()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), IdentityError> {
    let tmp = path.with_extension("bin.tmp");
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(io_err(&tmp, source)),
    }
    {
        let mut file = open_identity_file(&tmp)?;
        use std::io::Write;
        file.write_all(bytes).map_err(|source| IdentityError::Io {
            path: tmp.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| IdentityError::Io {
            path: tmp.clone(),
            source,
        })?;
    }
    restrict_identity_permissions(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|source| IdentityError::Io {
        path: path.to_owned(),
        source,
    })?;
    restrict_identity_permissions(path)?;
    if let Some(parent) = path.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn io_err(path: &Path, source: io::Error) -> IdentityError {
    IdentityError::Io {
        path: path.to_owned(),
        source,
    }
}

#[cfg(unix)]
fn open_identity_file(path: &Path) -> Result<std::fs::File, IdentityError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| io_err(path, source))
}

#[cfg(not(unix))]
fn open_identity_file(path: &Path) -> Result<std::fs::File, IdentityError> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_err(path, source))
}

#[cfg(unix)]
fn restrict_identity_permissions(path: &Path) -> Result<(), IdentityError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|source| io_err(path, source))
}

#[cfg(not(unix))]
fn restrict_identity_permissions(_path: &Path) -> Result<(), IdentityError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_identity_path_uses_override_before_platform_directory() {
        let config = crate::ThespanConfig::new("example", "/example", "EXAMPLE_IDENTITY_DIR");
        let override_dir = Path::new("override");
        let path = config
            .resolve_identity_path(
                |name| {
                    assert_eq!(name, "EXAMPLE_IDENTITY_DIR");
                    Some(override_dir.as_os_str().to_owned())
                },
                || panic!("the override must not need a platform directory"),
            )
            .unwrap();
        assert_eq!(path, override_dir.join("identity.bin"));

        let data_dir = Path::new("data");
        let path = config
            .resolve_identity_path(|_| None, || Some(data_dir.to_owned()))
            .unwrap();
        assert_eq!(path, data_dir.join("example").join("identity.bin"));
    }

    #[test]
    fn identity_app_id_cannot_escape_the_platform_data_directory() {
        let config = ThespanConfig::new("../example", "/example", "EXAMPLE_IDENTITY_DIR");
        assert!(matches!(
            config.resolve_identity_path(|_| None, || Some(PathBuf::from("data"))),
            Err(IdentityError::InvalidAppId(_))
        ));
    }
}

#[cfg(all(test, unix))]
mod unix_permissions {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thespan-id-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_and_existing_identity_are_0600() {
        let dir = scratch("perms");
        let path = dir.join(IDENTITY_FILE_NAME);
        load_or_create_keypair(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        load_or_create_keypair(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_temporary_file_is_replaced_safely() {
        let dir = scratch("stale-tmp");
        let path = dir.join(IDENTITY_FILE_NAME);
        std::fs::write(path.with_extension("bin.tmp"), b"stale").unwrap();

        load_or_create_keypair(&path).unwrap();

        assert!(path.exists());
        assert!(!path.with_extension("bin.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
