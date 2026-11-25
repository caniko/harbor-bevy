//! Caller-driven typed persistence for Steam Cloud and test backends.

use rkyv::{
    Archive, Deserialize, Serialize,
    api::high::{HighSerializer, HighValidator},
    bytecheck::CheckBytes,
    de::Pool,
    rancor::{Error, Strategy},
    ser::allocator::ArenaHandle,
    util::AlignedVec,
};
use std::collections::HashMap;

use crate::watchdog::SteamError;

#[derive(Debug, thiserror::Error)]
pub enum CloudSyncError {
    #[error("cloud filename must not be blank or contain NUL")]
    InvalidFilename,
    #[error("cloud file exceeds the Steam size limit")]
    FileTooLarge,
    #[error("could not serialize cloud data: {0}")]
    Serialize(#[source] Error),
    #[error("invalid cloud archive: {0}")]
    Deserialize(#[source] Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Steam(#[from] SteamError),
}

pub trait CloudStorageBackend {
    /// `None` means the file does not exist, not that a read failed.
    fn read(&mut self, filename: &str) -> Result<Option<Vec<u8>>, CloudSyncError>;
    fn write(&mut self, filename: &str, bytes: &[u8]) -> Result<(), CloudSyncError>;
}

pub fn save_typed(
    backend: &mut (impl CloudStorageBackend + ?Sized),
    filename: &str,
    value: &impl for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, Error>>,
) -> Result<(), CloudSyncError> {
    validate_filename(filename)?;
    let bytes = rkyv::to_bytes::<Error>(value).map_err(CloudSyncError::Serialize)?;
    backend.write(filename, &bytes)
}

/// Deserialize a checked archive. The caller is responsible for schema versions.
pub fn load_typed<T>(
    backend: &mut (impl CloudStorageBackend + ?Sized),
    filename: &str,
) -> Result<Option<T>, CloudSyncError>
where
    T: Archive,
    T::Archived:
        for<'a> CheckBytes<HighValidator<'a, Error>> + Deserialize<T, Strategy<Pool, Error>>,
{
    validate_filename(filename)?;
    backend
        .read(filename)?
        .map(|bytes| {
            // A backend's Vec<u8> need not satisfy rkyv's archive alignment.
            let mut aligned = AlignedVec::<16>::with_capacity(bytes.len());
            aligned.extend_from_slice(&bytes);
            rkyv::from_bytes::<T, Error>(&aligned).map_err(CloudSyncError::Deserialize)
        })
        .transpose()
}

fn validate_filename(filename: &str) -> Result<(), CloudSyncError> {
    if filename.trim().is_empty() || filename.contains('\0') {
        return Err(CloudSyncError::InvalidFilename);
    }
    Ok(())
}

#[derive(Default)]
pub struct InMemoryCloudBackend {
    files: HashMap<String, Vec<u8>>,
}

impl CloudStorageBackend for InMemoryCloudBackend {
    fn read(&mut self, filename: &str) -> Result<Option<Vec<u8>>, CloudSyncError> {
        validate_filename(filename)?;
        Ok(self.files.get(filename).cloned())
    }

    fn write(&mut self, filename: &str, bytes: &[u8]) -> Result<(), CloudSyncError> {
        validate_filename(filename)?;
        self.files.insert(filename.to_owned(), bytes.to_vec());
        Ok(())
    }
}

#[cfg(all(feature = "steam", not(any(target_os = "android", target_os = "ios"))))]
mod steam {
    use std::io::{Read, Write};

    use super::*;
    use crate::watchdog::{SteamWatchdog, steam_error_from_debug};

    const MAX_FILE_BYTES: usize = 100 * 1024 * 1024;

    /// Steam's safe streaming API acknowledges writes locally; it does not
    /// expose the stream-close result or confirm propagation to other devices.
    pub struct SteamCloudBackend<'a> {
        client: &'a bevy_steamworks::Client,
        watchdog: &'a mut SteamWatchdog,
    }

    impl<'a> SteamCloudBackend<'a> {
        pub fn new(client: &'a bevy_steamworks::Client, watchdog: &'a mut SteamWatchdog) -> Self {
            Self { client, watchdog }
        }
    }

    impl CloudStorageBackend for SteamCloudBackend<'_> {
        fn read(&mut self, filename: &str) -> Result<Option<Vec<u8>>, CloudSyncError> {
            validate_filename(filename)?;
            let bytes = self.watchdog.call_result("cloud.read", || {
                let file = self.client.remote_storage().file(filename);
                if !file.exists() {
                    return Ok(None);
                }
                let mut bytes = Vec::new();
                file.read()
                    .take(MAX_FILE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(steam_error_from_debug)?;
                Ok(Some(bytes))
            })?;
            if bytes
                .as_ref()
                .is_some_and(|bytes| bytes.len() > MAX_FILE_BYTES)
            {
                return Err(CloudSyncError::FileTooLarge);
            }
            Ok(bytes)
        }

        fn write(&mut self, filename: &str, bytes: &[u8]) -> Result<(), CloudSyncError> {
            validate_filename(filename)?;
            if bytes.len() > MAX_FILE_BYTES {
                return Err(CloudSyncError::FileTooLarge);
            }
            self.watchdog.call_result("cloud.write", || {
                self.client
                    .remote_storage()
                    .file(filename)
                    .write()
                    .write_all(bytes)
                    .map_err(steam_error_from_debug)
            })?;
            Ok(())
        }
    }
}

#[cfg(all(feature = "steam", not(any(target_os = "android", target_os = "ios"))))]
pub use steam::SteamCloudBackend;
