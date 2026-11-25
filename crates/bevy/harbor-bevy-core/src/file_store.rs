use std::{
    fs, io,
    path::{Path, PathBuf},
};

use bevy::prelude::Resource;
use rkyv::{
    Archive, Deserialize, Serialize,
    api::high::{HighDeserializer, HighSerializer, HighValidator},
    bytecheck::CheckBytes,
    rancor::Error,
    ser::allocator::ArenaHandle,
    util::AlignedVec,
};

use crate::{AppIdentity, write_then_rename};

/// A typed, file-backed key-value store rooted in an application-controlled directory.
///
/// Each key maps to `{key}.bin`. The default store does no I/O: writes and removals
/// are no-ops, and reads report not found. Callers own the archived data schema
/// and must keep the root private to trusted writers.
#[derive(Debug, Default, Resource)]
pub struct FileStore {
    dir: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum FileStoreError {
    #[error("invalid file store key: {0:?}")]
    InvalidKey(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("serialization error: {0}")]
    Serialize(#[source] Error),
    #[error("deserialization error: {0}")]
    Deserialize(#[source] Error),
}

impl FileStore {
    pub fn new(identity: &AppIdentity) -> Self {
        Self::from_dir(identity.data_dir())
    }

    pub fn from_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
        }
    }

    pub fn root_dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    pub fn is_disabled(&self) -> bool {
        self.dir.is_none()
    }

    /// Serialize the value and atomically replace any previous value.
    pub fn set<T>(&self, key: &str, value: &T) -> Result<(), FileStoreError>
    where
        T: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, Error>>,
    {
        let Some(path) = self.key_path(key)? else {
            return Ok(());
        };
        let bytes = rkyv::to_bytes::<Error>(value).map_err(FileStoreError::Serialize)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let temporary = path.with_file_name(format!(".store-{}.tmp", uuid::Uuid::new_v4()));
        write_then_rename(&temporary, &path, &bytes)?;
        Ok(())
    }

    /// Load and validate the archive associated with `key`.
    pub fn get<T>(&self, key: &str) -> Result<T, FileStoreError>
    where
        T: Archive,
        T::Archived:
            for<'a> CheckBytes<HighValidator<'a, Error>> + Deserialize<T, HighDeserializer<Error>>,
    {
        let path = self
            .key_path(key)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "file store is disabled"))?;
        let bytes = fs::read(path)?;
        let mut aligned = AlignedVec::<16>::with_capacity(bytes.len());
        aligned.extend_from_slice(&bytes);
        rkyv::from_bytes::<T, Error>(&aligned).map_err(FileStoreError::Deserialize)
    }

    /// Remove the stored value. A missing value is not an error.
    pub fn remove(&self, key: &str) -> Result<(), FileStoreError> {
        let Some(path) = self.key_path(key)? else {
            return Ok(());
        };
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn key_path(&self, key: &str) -> Result<Option<PathBuf>, FileStoreError> {
        if matches!(key, "" | "." | "..") || key.contains(['/', '\\', ':', '\0']) {
            return Err(FileStoreError::InvalidKey(key.to_owned()));
        }
        Ok(self.dir.as_ref().map(|dir| dir.join(format!("{key}.bin"))))
    }
}
