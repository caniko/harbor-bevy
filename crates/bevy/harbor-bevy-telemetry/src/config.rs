use std::path::PathBuf;

use bevy::prelude::Resource;
use diagnostics_policy::{Consent, ProfileKind, Purpose, RetentionCap};
use telemetry_protocol::ClientHeader;

#[derive(Clone, Debug, Resource)]
pub struct TelemetryConfig {
    /// Set to `Adult` only once the application's profile policy allows it.
    pub profile: ProfileKind,
    /// Revocation should go through `SessionRecorder::withdraw_consent` so the
    /// active worker is notified synchronously.
    pub consent: Consent,
    pub purpose: Purpose,
    pub retention: RetentionCap,
    pub client: ClientHeader,
    /// Maximum events per batch, fixed when the recorder is constructed.
    /// A value of zero is treated as one.
    pub batch_size: usize,
    pub flush_interval_secs: u64,
    pub upload_interval_secs: u64,
    pub max_retries: u32,
    pub retry_backoff_base_secs: u64,
    /// A dedicated, trusted application-private directory. Do not set this
    /// from event data or use a directory writable by untrusted processes.
    pub storage_dir: Option<PathBuf>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            profile: ProfileKind::Child,
            consent: Consent::default(),
            purpose: Purpose::GameplayLogs,
            retention: RetentionCap::default(),
            client: ClientHeader {
                name: String::new(),
                version: String::new(),
            },
            batch_size: 500,
            flush_interval_secs: 30,
            upload_interval_secs: 300,
            max_retries: 3,
            retry_backoff_base_secs: 1,
            storage_dir: None,
        }
    }
}

impl TelemetryConfig {
    pub fn may_collect(&self) -> bool {
        diagnostics_policy::may_collect(self.profile, self.consent, self.purpose)
    }
}
