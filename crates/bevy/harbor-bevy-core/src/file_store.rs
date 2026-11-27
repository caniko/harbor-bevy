use std::{
    fs::{self, File},
    io::{self, ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::ffi::{OsStr, OsString};
#[cfg(not(unix))]
use std::fs::OpenOptions;

use bevy::prelude::Resource;
use rkyv::{
    Archive, Deserialize, Serialize,
    api::high::{HighDeserializer, HighSerializer, HighValidator},
    bytecheck::CheckBytes,
    rancor::Error,
    ser::allocator::ArenaHandle,
    util::AlignedVec,
};

#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, renameat, statat, unlinkat};

use crate::{AppIdentity, open_sync_directory, parent_dir, write_then_rename};

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
        let bytes = fs::read(self.read_path(key)?)?;
        decode(&bytes)
    }

    /// Load and validate an archive, rejecting files larger than `max_bytes` before decoding.
    pub fn get_bounded<T>(&self, key: &str, max_bytes: usize) -> Result<T, FileStoreError>
    where
        T: Archive,
        T::Archived:
            for<'a> CheckBytes<HighValidator<'a, Error>> + Deserialize<T, HighDeserializer<Error>>,
    {
        let bytes = read_bounded(File::open(self.read_path(key)?)?, max_bytes)?;
        decode(&bytes)
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

    fn read_path(&self, key: &str) -> Result<PathBuf, FileStoreError> {
        self.key_path(key)?
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "file store is disabled").into())
    }

    fn key_path(&self, key: &str) -> Result<Option<PathBuf>, FileStoreError> {
        if matches!(key, "" | "." | "..") || key.contains(['/', '\\', ':', '\0']) {
            return Err(FileStoreError::InvalidKey(key.to_owned()));
        }
        Ok(self.dir.as_ref().map(|dir| dir.join(format!("{key}.bin"))))
    }
}

fn decode<T>(bytes: &[u8]) -> Result<T, FileStoreError>
where
    T: Archive,
    T::Archived:
        for<'a> CheckBytes<HighValidator<'a, Error>> + Deserialize<T, HighDeserializer<Error>>,
{
    let mut aligned = AlignedVec::<16>::with_capacity(bytes.len());
    aligned.extend_from_slice(bytes);
    rkyv::from_bytes::<T, Error>(&aligned).map_err(FileStoreError::Deserialize)
}

fn read_bounded(file: impl Read, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "file byte limit exceeded",
        ));
    }
    Ok(bytes)
}

/// Single-writer atomic storage for one opaque byte payload.
///
/// Supported on Unix. The parent directory must exist and be controlled by trusted writers
/// on a local filesystem that supports file locks, atomic rename, and directory
/// sync. On Unix, all entry operations and the directory sync use the same open directory,
/// even if that directory is renamed. Writers must use the same store and must not move or
/// unlink the lock file. A forked child must drop inherited handles and reopen the store;
/// reopening still requires acquiring the lock.
#[derive(Debug)]
pub struct DurableFileStore {
    #[cfg(unix)]
    names: StoreNames,
    #[cfg(not(unix))]
    path: PathBuf,
    #[cfg(not(unix))]
    stage: PathBuf,
    parent: File,
    _lock: File,
    owner_pid: u32,
    uncertain: bool,
    #[cfg(all(test, unix))]
    fault: Option<CommitFault>,
}

#[cfg(unix)]
#[derive(Debug)]
struct StoreNames {
    payload_name: OsString,
    lock_name: OsString,
    stage_name: OsString,
}

#[derive(Debug, thiserror::Error)]
pub enum DurableStoreError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("store handle belongs to a different process; drop it and reopen")]
    ProcessChanged,
    #[error("store durability is uncertain; reopen required")]
    Uncertain,
    #[error("rename failed; store durability is uncertain: {0}")]
    Rename(#[source] io::Error),
    #[error("payload sync failed; store durability is uncertain: {0}")]
    PayloadSync(#[source] io::Error),
    #[error("directory sync failed; store durability is uncertain: {0}")]
    DirectorySync(#[source] io::Error),
}

impl DurableStoreError {
    /// Return the underlying I/O error kind, or [`ErrorKind::Other`] for handle state errors.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Io(error)
            | Self::Rename(error)
            | Self::PayloadSync(error)
            | Self::DirectorySync(error) => error.kind(),
            Self::ProcessChanged | Self::Uncertain => ErrorKind::Other,
        }
    }
}

impl DurableFileStore {
    /// Inclusive size limit for reads and writes (16 MiB).
    pub const MAX_BYTES: usize = 16 * 1024 * 1024;

    /// Acquire the sibling `.lock` without waiting and discard any stale `.next`.
    /// The `.lock` and `.next` extensions are reserved for these sidecar files.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "store needs a file name"))?;
        if path.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("lock") || extension.eq_ignore_ascii_case("next")
        }) {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "store file extension is reserved",
            ));
        }
        let sibling = |suffix: &str| {
            let mut sibling = name.to_owned();
            sibling.push(suffix);
            sibling
        };
        #[cfg(unix)]
        let (parent, names, lock) = {
            let parent = open_sync_directory(parent_dir(path))?;
            let names = StoreNames {
                payload_name: name.to_owned(),
                lock_name: sibling(".lock"),
                stage_name: sibling(".next"),
            };
            let lock = open_regular_at(&parent, &names.lock_name, OFlags::CREATE | OFlags::WRONLY)?;
            (parent, names, lock)
        };
        #[cfg(not(unix))]
        let (parent, dir, lock) = {
            let dir = parent_dir(path).canonicalize()?;
            let parent = open_sync_directory(&dir)?;
            let lock_path = dir.join(sibling(".lock"));
            check_regular_file(&lock_path)?;
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(lock_path)?;
            (parent, dir, lock)
        };
        lock.try_lock().map_err(io::Error::from)?;
        let store = Self {
            #[cfg(unix)]
            names,
            #[cfg(not(unix))]
            path: dir.join(name),
            #[cfg(not(unix))]
            stage: dir.join(sibling(".next")),
            parent,
            _lock: lock,
            owner_pid: std::process::id(),
            uncertain: false,
            #[cfg(all(test, unix))]
            fault: None,
        };
        store.remove_stage()?;
        Ok(store)
    }

    /// Whether a publication or recovery sync failed on this handle and reopening is required.
    /// A fresh handle starts clear; that does not confirm crash durability.
    pub fn is_uncertain(&self) -> bool {
        self.uncertain
    }

    /// Inspect the currently visible payload, if present. After an uncertain commit, drop
    /// and reopen the store to inspect it. Inspection does not confirm crash durability:
    /// call [`Self::recover_durable`] or successfully commit again to establish it.
    pub fn load(&self) -> Result<Option<Vec<u8>>, DurableStoreError> {
        self.check_usable()?;
        Ok(self
            .open_payload(false)?
            .map(|file| read_bounded(file, Self::MAX_BYTES))
            .transpose()?)
    }

    /// Replace the entire payload. Staging failures leave the old payload in place.
    /// A failure during rename or directory sync latches uncertainty.
    pub fn commit(&mut self, bytes: &[u8]) -> Result<(), DurableStoreError> {
        self.check_usable()?;
        if bytes.len() > Self::MAX_BYTES {
            return Err(io::Error::new(ErrorKind::InvalidInput, "file byte limit exceeded").into());
        }
        self.check_destination()?;
        let mut stage = self.create_stage()?;
        let staged = self.write_stage(&mut stage, bytes);
        drop(stage);
        if let Err(error) = staged {
            let _ = self.remove_stage();
            return Err(error.into());
        }
        if let Err(error) = self.publish_stage() {
            self.uncertain = true;
            let _ = self.remove_stage();
            return Err(DurableStoreError::Rename(error));
        }
        self.sync_parent()
    }

    /// Confirm crash durability of the currently visible payload (or its absence).
    /// Reopen first if the previous handle reported uncertainty. A present payload
    /// is opened for writing so it can be synced, but is not modified.
    /// If either the payload or directory sync fails, reopen before trying again.
    pub fn recover_durable(&mut self) -> Result<Option<Vec<u8>>, DurableStoreError> {
        self.check_usable()?;
        let bytes = if let Some(mut file) = self.open_payload(true)? {
            let bytes = read_bounded(&mut file, Self::MAX_BYTES)?;
            if let Err(error) = file.sync_all() {
                self.uncertain = true;
                return Err(DurableStoreError::PayloadSync(error));
            }
            Some(bytes)
        } else {
            None
        };
        self.sync_parent()?;
        Ok(bytes)
    }

    fn check_usable(&self) -> Result<(), DurableStoreError> {
        if self.owner_pid != std::process::id() {
            return Err(DurableStoreError::ProcessChanged);
        }
        if self.uncertain {
            return Err(DurableStoreError::Uncertain);
        }
        Ok(())
    }

    fn open_payload(&self, writable: bool) -> io::Result<Option<File>> {
        #[cfg(unix)]
        let result = open_regular_at(
            &self.parent,
            &self.names.payload_name,
            if writable {
                OFlags::RDWR
            } else {
                OFlags::RDONLY
            },
        );
        #[cfg(not(unix))]
        let result = {
            check_regular_file(&self.path)?;
            OpenOptions::new()
                .read(true)
                .write(writable)
                .open(&self.path)
        };
        optional_entry(result)
    }

    fn check_destination(&self) -> io::Result<()> {
        // A directory cannot be replaced by a regular file. Reject it before attempting rename.
        #[cfg(unix)]
        let is_dir = statat(
            &self.parent,
            &self.names.payload_name,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Directory)
        .map_err(io::Error::from);
        #[cfg(not(unix))]
        let is_dir = fs::symlink_metadata(&self.path).map(|metadata| metadata.is_dir());
        if optional_entry(is_dir)? == Some(true) {
            return Err(ErrorKind::IsADirectory.into());
        }
        Ok(())
    }

    fn remove_stage(&self) -> io::Result<()> {
        #[cfg(unix)]
        let result = unlinkat(&self.parent, &self.names.stage_name, AtFlags::empty())
            .map_err(io::Error::from);
        #[cfg(not(unix))]
        let result = fs::remove_file(&self.stage);
        optional_entry(result).map(|_| ())
    }

    fn create_stage(&self) -> io::Result<File> {
        #[cfg(unix)]
        {
            Ok(openat(
                &self.parent,
                &self.names.stage_name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o666),
            )
            .map_err(io::Error::from)?
            .into())
        }
        #[cfg(not(unix))]
        File::create_new(&self.stage)
    }

    fn write_stage(&self, stage: &mut File, bytes: &[u8]) -> io::Result<()> {
        #[cfg(all(test, unix))]
        self.inject_fault(CommitFault::Write)?;
        stage.write_all(bytes)?;
        stage.sync_all()
    }

    fn publish_stage(&self) -> io::Result<()> {
        #[cfg(all(test, unix))]
        self.inject_fault(CommitFault::Rename)?;
        #[cfg(unix)]
        {
            renameat(
                &self.parent,
                &self.names.stage_name,
                &self.parent,
                &self.names.payload_name,
            )
            .map_err(io::Error::from)
        }
        #[cfg(not(unix))]
        fs::rename(&self.stage, &self.path)
    }

    fn sync_parent(&mut self) -> Result<(), DurableStoreError> {
        #[cfg(all(test, unix))]
        let result = self
            .inject_fault(CommitFault::DirectorySync)
            .and_then(|()| self.parent.sync_all());
        #[cfg(not(all(test, unix)))]
        let result = self.parent.sync_all();
        if let Err(error) = result {
            self.uncertain = true;
            return Err(DurableStoreError::DirectorySync(error));
        }
        Ok(())
    }

    #[cfg(all(test, unix))]
    fn inject_fault(&self, phase: CommitFault) -> io::Result<()> {
        if self.fault == Some(phase) {
            return Err(io::Error::other("injected I/O failure"));
        }
        Ok(())
    }

    #[cfg(all(test, unix))]
    fn set_owner_pid(&mut self, owner_pid: u32) {
        self.owner_pid = owner_pid;
    }
}

fn optional_entry<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn open_regular_at(parent: &File, name: &OsStr, flags: OFlags) -> io::Result<File> {
    // NONBLOCK lets us reject a FIFO without waiting for another process to open it.
    let file = File::from(
        openat(
            parent,
            name,
            flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::from_raw_mode(0o666),
        )
        .map_err(|error| {
            if error == rustix::io::Errno::LOOP {
                io::Error::new(ErrorKind::InvalidData, "expected a regular file")
            } else {
                error.into()
            }
        })?,
    );
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "expected a regular file",
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn check_regular_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            ErrorKind::InvalidData,
            "expected a regular file",
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(all(test, unix))]
#[derive(Clone, Copy, Debug, PartialEq)]
enum CommitFault {
    Write,
    Rename,
    DirectorySync,
}

#[cfg(all(test, unix))]
#[test]
fn commit_and_load_use_the_original_directory_after_it_moves() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("dir_a");
    let moved = root.path().join("dir_a_moved");
    fs::create_dir(&dir).unwrap();
    let mut store = DurableFileStore::open(dir.join("state.bin")).unwrap();

    fs::rename(&dir, &moved).unwrap();
    fs::create_dir(&dir).unwrap();
    store.commit(b"pinned").unwrap();

    assert!(fs::read_dir(&dir).unwrap().next().is_none());
    assert_eq!(fs::read(moved.join("state.bin")).unwrap(), b"pinned");
    assert_eq!(store.load().unwrap().as_deref(), Some(b"pinned".as_slice()));
}

#[cfg(all(test, unix))]
#[test]
fn rename_failure_latches_uncertainty() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DurableFileStore::open(dir.path().join("state")).unwrap();
    store.commit(b"old").unwrap();
    store.fault = Some(CommitFault::Rename);

    assert!(matches!(
        store.commit(b"new"),
        Err(DurableStoreError::Rename(_))
    ));
    assert!(store.is_uncertain());
    assert!(matches!(store.load(), Err(DurableStoreError::Uncertain)));
    assert!(matches!(
        store.commit(b"retry"),
        Err(DurableStoreError::Uncertain)
    ));
    assert_eq!(fs::read(dir.path().join("state")).unwrap(), b"old");
    assert!(!dir.path().join("state.next").exists());
}

#[cfg(all(test, unix))]
#[test]
fn staging_write_failure_leaves_the_store_usable() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DurableFileStore::open(dir.path().join("state")).unwrap();
    store.commit(b"old").unwrap();
    store.fault = Some(CommitFault::Write);

    assert!(matches!(
        store.commit(b"failed"),
        Err(DurableStoreError::Io(_))
    ));
    assert!(!store.is_uncertain());
    assert_eq!(store.load().unwrap().as_deref(), Some(b"old".as_slice()));
    store.fault = None;
    store.commit(b"retry").unwrap();
    assert_eq!(store.load().unwrap().as_deref(), Some(b"retry".as_slice()));
}

#[cfg(all(test, unix))]
#[test]
fn inspection_after_reopen_does_not_establish_durability() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state");
    let mut store = DurableFileStore::open(&path).unwrap();
    store.commit(b"old").unwrap();

    store.fault = Some(CommitFault::DirectorySync);
    assert!(matches!(
        store.commit(b"new"),
        Err(DurableStoreError::DirectorySync(_))
    ));
    assert!(store.is_uncertain());
    assert!(store.load().is_err());
    assert!(store.commit(b"must not replace").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"new");

    drop(store);
    let mut store = DurableFileStore::open(&path).unwrap();
    assert!(!store.is_uncertain());
    assert_eq!(store.load().unwrap().as_deref(), Some(b"new".as_slice()));
    store.fault = Some(CommitFault::DirectorySync);
    assert!(matches!(
        store.recover_durable(),
        Err(DurableStoreError::DirectorySync(_))
    ));
    assert!(store.is_uncertain());
    assert!(matches!(
        store.recover_durable(),
        Err(DurableStoreError::Uncertain)
    ));

    drop(store);
    let mut store = DurableFileStore::open(&path).unwrap();
    assert_eq!(
        store.recover_durable().unwrap().as_deref(),
        Some(b"new".as_slice())
    );
    assert!(!store.is_uncertain());
}

#[cfg(all(test, unix))]
#[test]
fn inherited_handle_rejects_io() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DurableFileStore::open(dir.path().join("state")).unwrap();
    store.set_owner_pid(std::process::id() ^ 1);

    assert!(matches!(
        store.load(),
        Err(DurableStoreError::ProcessChanged)
    ));
    assert!(matches!(
        store.commit(b"child"),
        Err(DurableStoreError::ProcessChanged)
    ));
    assert!(matches!(
        store.recover_durable(),
        Err(DurableStoreError::ProcessChanged)
    ));
    assert!(!dir.path().join("state").exists());
}
