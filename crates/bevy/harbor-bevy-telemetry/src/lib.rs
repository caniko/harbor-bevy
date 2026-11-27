//! Opt-in telemetry for caller-defined event payloads. Insert `TelemetryConfig`
//! and add `TelemetryPlugin::<E>` to install a session recorder. Applications
//! record through `SessionRecorder::record`; no events are collected implicitly.
//!
//! Disk storage and upload are separately opt-in. To enable upload, insert a
//! `TelemetryUploader<E>` built with a caller-owned Tokio runtime. To revoke
//! consent, call `SessionRecorder::withdraw_consent` with that uploader (if
//! installed) rather than just editing the config between frames.

pub mod batcher;
pub mod config;
pub mod plugin;
pub mod recorder;
pub mod uploader;

pub use batcher::EnvelopeBatcher;
pub use config::TelemetryConfig;
pub use plugin::{FlushTimer, TelemetryPlugin, TelemetrySystems, UploadTimer};
pub use recorder::{RecordError, SessionRecorder, StorageError, cleanup_expired};
pub use uploader::{
    TelemetryClientTransport, TelemetryTransport, TelemetryUploader, UploadError, UploadOutcome,
};

#[cfg(test)]
mod tests;
