use std::{
    marker::PhantomData,
    time::{Instant, SystemTime},
};

use bevy::{app::AppExit, prelude::*};
use rkyv::{
    Serialize, api::high::HighSerializer, rancor::Error as ArchiveError,
    ser::allocator::ArenaHandle, util::AlignedVec,
};
use telemetry_protocol::{Batch, SessionId};

use crate::{SessionRecorder, TelemetryConfig, TelemetryUploader, UploadError, cleanup_expired};

#[derive(Resource)]
pub struct FlushTimer {
    pub last_flush: Instant,
}

impl Default for FlushTimer {
    fn default() -> Self {
        Self {
            last_flush: Instant::now(),
        }
    }
}

#[derive(Resource)]
pub struct UploadTimer {
    pub last_upload: Instant,
}

impl Default for UploadTimer {
    fn default() -> Self {
        Self {
            last_upload: Instant::now(),
        }
    }
}

/// Installs a recorder for a caller-defined session and payload. Insert the
/// configuration before adding the plugin to set the recorder's batch size.
pub struct TelemetryPlugin<E> {
    session: SessionId,
    event: PhantomData<fn() -> E>,
}

impl<E> TelemetryPlugin<E> {
    pub fn new(session: SessionId) -> Self {
        Self {
            session,
            event: PhantomData,
        }
    }
}

impl<E: Send + Sync + 'static> Plugin for TelemetryPlugin<E>
where
    Batch<E>: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, ArchiveError>>,
{
    fn build(&self, app: &mut App) {
        app.init_resource::<TelemetryConfig>();
        let recorder = SessionRecorder::<E>::new(
            self.session.clone(),
            app.world().resource::<TelemetryConfig>(),
        );
        app.insert_resource(recorder)
            .init_resource::<FlushTimer>()
            .init_resource::<UploadTimer>()
            .add_systems(PostStartup, clean_storage)
            .add_systems(Update, maintain::<E>)
            .add_systems(Last, flush_on_exit::<E>);
    }
}

fn clean_storage(config: Res<TelemetryConfig>) {
    clean(&config);
}

fn clean(config: &TelemetryConfig) {
    if let Err(error) = cleanup_expired(config, SystemTime::now()) {
        warn!("Telemetry retention cleanup failed ({:?})", error.kind());
    }
}

fn maintain<E: Send + Sync + 'static>(
    config: Res<TelemetryConfig>,
    mut recorder: ResMut<SessionRecorder<E>>,
    // Upload is an optional service; disk-only recording is supported.
    uploader: Option<Res<TelemetryUploader<E>>>,
    mut flush: ResMut<FlushTimer>,
    mut upload: ResMut<UploadTimer>,
) where
    Batch<E>: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, ArchiveError>>,
{
    let uploader = uploader.as_deref();
    if let Some(uploader) = uploader {
        uploader.update_config(&config);
    }
    let now = Instant::now();
    let should_flush = (now - flush.last_flush).as_secs() >= config.flush_interval_secs;
    let should_upload =
        uploader.is_some() && (now - upload.last_upload).as_secs() >= config.upload_interval_secs;
    if should_flush {
        clean(&config);
        flush.last_flush = now;
    }
    if !config.may_collect() {
        recorder.discard_pending();
        return;
    }
    let upload_enabled = uploader.is_some_and(|uploader| !uploader.is_refused());
    if (config.storage_dir.is_some() || upload_enabled)
        && (should_flush || should_upload || recorder.is_full())
    {
        match recorder.flush_batch(&config) {
            Ok(Some(batch)) if upload_enabled => {
                if let Some(uploader) = uploader
                    && let Err(error) = uploader.stage_batch(batch)
                    && error != UploadError::Refused
                {
                    warn!("{error}");
                }
            }
            Ok(_) => {}
            Err(error) => warn!("{error}"),
        }
    }
    if should_upload {
        if let Some(uploader) = uploader {
            uploader.release_staged();
        }
        upload.last_upload = now;
    }
}

fn flush_on_exit<E: Send + Sync + 'static>(
    mut exit: MessageReader<AppExit>,
    config: Res<TelemetryConfig>,
    mut recorder: ResMut<SessionRecorder<E>>,
) where
    Batch<E>: for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, ArchiveError>>,
{
    if exit.read().count() != 0
        && config.storage_dir.is_some()
        && let Err(error) = recorder.flush_batch(&config)
    {
        warn!("{error}");
    }
}
