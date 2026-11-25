use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use bevy::prelude::Resource;
use diagnostics_policy::RetentionKind;
use rkyv::{
    Serialize, api::high::HighSerializer, rancor::Error as ArchiveError,
    ser::allocator::ArenaHandle, util::AlignedVec,
};
use telemetry_protocol::{Batch, EventMetadata, SessionId, Timestamp};
use uuid::Uuid;

use crate::{EnvelopeBatcher, TelemetryConfig, TelemetryUploader};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RecordError {
    #[error("telemetry collection is not allowed")]
    NotAllowed,
    #[error("pending telemetry batch is full")]
    BatchFull,
    #[error("session sequence exhausted")]
    SequenceExhausted,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("telemetry storage I/O failed")]
    Io(#[from] io::Error),
    #[error("telemetry archive encoding failed")]
    Archive(#[from] ArchiveError),
}

/// Session-scoped recorder for application-defined events. The local sequence
/// counts accepted events; it is not part of the wire envelope.
#[derive(Resource)]
pub struct SessionRecorder<E: Send + Sync + 'static> {
    session: SessionId,
    next_sequence: u64,
    batcher: EnvelopeBatcher<E>,
}

impl<E: Send + Sync + 'static> SessionRecorder<E> {
    pub fn new(session: SessionId, config: &TelemetryConfig) -> Self {
        Self {
            session,
            next_sequence: 0,
            batcher: EnvelopeBatcher::new(config.batch_size),
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session
    }

    pub fn pending_len(&self) -> usize {
        self.batcher.len()
    }

    pub(crate) fn is_full(&self) -> bool {
        self.batcher.is_full()
    }

    /// Record under the live policy, returning the event's zero-based sequence.
    /// A rejected event is not retained and does not consume a sequence number.
    pub fn record(&mut self, config: &TelemetryConfig, event: E) -> Result<u64, RecordError> {
        if !config.may_collect() {
            self.discard_pending();
            return Err(RecordError::NotAllowed);
        }
        let sequence = self.next_sequence;
        let next = sequence
            .checked_add(1)
            .ok_or(RecordError::SequenceExhausted)?;
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let metadata = EventMetadata {
            timestamp: Timestamp(u64::try_from(millis).unwrap_or(u64::MAX)),
            session: self.session.clone(),
            client: config.client.clone(),
        };
        self.batcher
            .push(event, metadata)
            .map_err(|_| RecordError::BatchFull)?;
        self.next_sequence = next;
        Ok(sequence)
    }

    /// Stop collection and invalidate queued/in-progress uploads before any
    /// optional disk deletion. Pass the session's uploader if one is installed.
    /// Already transmitted data cannot be recalled.
    pub fn withdraw_consent(
        &mut self,
        config: &mut TelemetryConfig,
        uploader: Option<&TelemetryUploader<E>>,
        delete_local: bool,
    ) -> io::Result<()> {
        let (consent, action) = diagnostics_policy::withdraw();
        config.consent = consent;
        if let Some(uploader) = uploader {
            uploader.update_config(config);
        }
        self.batcher.apply(action);
        if delete_local
            && diagnostics_policy::apply_local_delete().diagnostics
            && let Some(dir) = &config.storage_dir
        {
            remove_files(dir, |_| Ok(true))?;
        }
        Ok(())
    }

    pub(crate) fn discard_pending(&mut self) {
        self.batcher.withdraw_consent();
    }

    /// Persist the current batch if storage is configured, then return it.
    /// A storage failure leaves the batch pending for a later flush.
    pub fn flush_batch(
        &mut self,
        config: &TelemetryConfig,
    ) -> Result<Option<Batch<E>>, StorageError>
    where
        Batch<E>: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, ArchiveError>>,
    {
        if !config.may_collect() {
            self.discard_pending();
            return Ok(None);
        }
        if !self.batcher.is_empty()
            && let Some(dir) = &config.storage_dir
        {
            let bytes = rkyv::to_bytes(self.batcher.batch())?;
            write_archive(dir, &bytes)?;
        }
        Ok(self.batcher.drain())
    }
}

const PREFIX: &str = "telemetry-";
const ARCHIVE: &str = ".rkyv";
const PARTIAL: &str = ".partial";

fn write_archive(dir: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut dirs = DirBuilder::new();
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        dirs.mode(0o700);
        options.mode(0o600);
    }
    dirs.recursive(true).create(dir)?;
    check_directory(dir)?;
    let name = format!("{PREFIX}{}", Uuid::new_v4());
    let partial = dir.join(format!("{name}{PARTIAL}"));
    let archive = dir.join(format!("{name}{ARCHIVE}"));
    let mut file = options.write(true).create_new(true).open(&partial)?;
    let written = file.write_all(bytes);
    drop(file);
    let result = written.and_then(|()| fs::rename(&partial, archive));
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

/// Remove expired telemetry archives (and incomplete writes) using file
/// modification time, without following symlinks or removing other files.
pub fn cleanup_expired(config: &TelemetryConfig, now: SystemTime) -> io::Result<usize> {
    let Some(dir) = &config.storage_dir else {
        return Ok(0);
    };
    remove_files(dir, |entry| {
        Ok(diagnostics_policy::expired(
            RetentionKind::Log,
            entry.metadata()?.modified()?,
            now,
            config.retention,
        ))
    })
}

fn check_directory(dir: &Path) -> io::Result<()> {
    if fs::symlink_metadata(dir)?.is_dir() {
        Ok(())
    } else {
        Err(io::Error::from(io::ErrorKind::NotADirectory))
    }
}

fn remove_files(
    dir: &Path,
    should_remove: impl Fn(&fs::DirEntry) -> io::Result<bool>,
) -> io::Result<usize> {
    match check_directory(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    }
    let mut removed = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let ours = name.to_str().and_then(|name| {
            let name = name.strip_prefix(PREFIX)?;
            let id = name
                .strip_suffix(ARCHIVE)
                .or_else(|| name.strip_suffix(PARTIAL))?;
            Uuid::try_parse(id).ok()
        });
        if ours.is_some() && entry.file_type()?.is_file() && should_remove(&entry)? {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}
