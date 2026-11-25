//! Bevy-independent diagnostics policy core.

use std::time::{Duration, SystemTime};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const LOG_RETENTION_DAYS: u32 = 14;
pub const CRASH_RETENTION_DAYS: u32 = 90;
pub const SAFETY_REPORT_RETENTION_DAYS: u32 = 180;
pub const DELETION_SLA_DAYS: u32 = 30;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Consent {
    #[default]
    Unknown,
    Granted,
    Denied,
    Withdrawn,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Purpose {
    GameplayLogs,
    CrashArtifacts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileKind {
    Child,
    Adult,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueAction {
    Keep,
    ClearPending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionKind {
    Log,
    Crash,
    SafetyReport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionCap {
    pub log_days: u32,
    pub crash_days: u32,
    pub safety_report_days: u32,
}

impl Default for RetentionCap {
    fn default() -> Self {
        Self {
            log_days: LOG_RETENTION_DAYS,
            crash_days: CRASH_RETENTION_DAYS,
            safety_report_days: SAFETY_REPORT_RETENTION_DAYS,
        }
    }
}

impl RetentionCap {
    pub fn clamp(self) -> Self {
        Self {
            log_days: self.log_days.min(LOG_RETENTION_DAYS),
            crash_days: self.crash_days.min(CRASH_RETENTION_DAYS),
            safety_report_days: self.safety_report_days.min(SAFETY_REPORT_RETENTION_DAYS),
        }
    }
}

pub fn may_collect(profile: ProfileKind, consent: Consent, _purpose: Purpose) -> bool {
    profile == ProfileKind::Adult && consent == Consent::Granted
}

pub fn withdraw() -> (Consent, QueueAction) {
    (Consent::Withdrawn, QueueAction::ClearPending)
}

pub fn expired(
    kind: RetentionKind,
    stored_at: SystemTime,
    now: SystemTime,
    cap: RetentionCap,
) -> bool {
    let cap = cap.clamp();
    let days = match kind {
        RetentionKind::Log => cap.log_days,
        RetentionKind::Crash => cap.crash_days,
        RetentionKind::SafetyReport => cap.safety_report_days,
    };
    now.duration_since(stored_at)
        .map(|d| d >= Duration::from_secs(u64::from(days) * 86_400))
        .unwrap_or(false)
}

#[derive(Clone, Copy, Debug)]
pub struct DeleteRequest {
    pub now: SystemTime,
}

impl DeleteRequest {
    pub fn server_deadline(self) -> SystemTime {
        self.now + Duration::from_secs(u64::from(DELETION_SLA_DAYS) * 86_400)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueState {
    Cleared,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalDelete {
    pub diagnostics: bool,
    pub replays: bool,
    pub pending_queue: QueueState,
}

pub fn apply_local_delete() -> LocalDelete {
    LocalDelete {
        diagnostics: true,
        replays: true,
        pending_queue: QueueState::Cleared,
    }
}

pub trait Signer {
    fn sign(&self, bytes: &[u8]) -> Vec<u8>;
}

pub fn deletion_canonical_bytes(request: &DeleteRequest) -> Vec<u8> {
    request
        .server_deadline()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_le_bytes()
        .to_vec()
}

pub fn sign_deletion(request: &DeleteRequest, signer: &impl Signer) -> Vec<u8> {
    signer.sign(&deletion_canonical_bytes(request))
}
