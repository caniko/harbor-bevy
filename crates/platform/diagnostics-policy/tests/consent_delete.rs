use std::time::{Duration, SystemTime, UNIX_EPOCH};

use diagnostics_policy::{
    Consent, DELETION_SLA_DAYS, DeleteRequest, LOG_RETENTION_DAYS, LocalDelete, ProfileKind,
    Purpose, QueueState, RetentionCap, RetentionKind, SAFETY_REPORT_RETENTION_DAYS, Signer,
    apply_local_delete, expired, may_collect, sign_deletion, withdraw,
};

struct Echo;

impl Signer for Echo {
    fn sign(&self, bytes: &[u8]) -> Vec<u8> {
        bytes.to_vec()
    }
}

fn t0() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn days(n: u32) -> Duration {
    Duration::from_secs(u64::from(n) * 86_400)
}

#[test]
fn child_cannot_opt_in() {
    assert!(!may_collect(
        ProfileKind::Child,
        Consent::Granted,
        Purpose::GameplayLogs
    ));
    assert!(!may_collect(
        ProfileKind::Child,
        Consent::Granted,
        Purpose::CrashArtifacts
    ));
}

#[test]
fn adult_default_and_unknown_are_off() {
    assert!(!may_collect(
        ProfileKind::Adult,
        Consent::Unknown,
        Purpose::GameplayLogs
    ));
    assert!(!may_collect(
        ProfileKind::Adult,
        Consent::Denied,
        Purpose::CrashArtifacts
    ));
    assert!(!may_collect(
        ProfileKind::Adult,
        Consent::Withdrawn,
        Purpose::GameplayLogs
    ));
}

#[test]
fn adult_granted_may_collect() {
    assert!(may_collect(
        ProfileKind::Adult,
        Consent::Granted,
        Purpose::GameplayLogs
    ));
}

#[test]
fn withdraw_clears_pending() {
    let (consent, action) = withdraw();
    assert_eq!(consent, Consent::Withdrawn);
    assert_eq!(action, diagnostics_policy::QueueAction::ClearPending);
    assert!(!may_collect(
        ProfileKind::Adult,
        consent,
        Purpose::GameplayLogs
    ));
}

#[test]
fn retention_expires_at_maxima() {
    let cap = RetentionCap::default();
    let start = t0();
    assert!(!expired(
        RetentionKind::Log,
        start,
        start + days(LOG_RETENTION_DAYS) - Duration::from_secs(1),
        cap
    ));
    assert!(expired(
        RetentionKind::Log,
        start,
        start + days(LOG_RETENTION_DAYS),
        cap
    ));
    assert!(expired(
        RetentionKind::Log,
        start,
        start + days(LOG_RETENTION_DAYS + 1),
        cap
    ));
    assert!(expired(RetentionKind::Crash, start, start + days(90), cap));
    assert!(!expired(
        RetentionKind::SafetyReport,
        start,
        start + days(SAFETY_REPORT_RETENTION_DAYS - 1),
        cap
    ));
}

#[test]
fn retention_cap_cannot_exceed_maxima() {
    let cap = RetentionCap {
        log_days: 30,
        crash_days: 200,
        safety_report_days: 400,
    }
    .clamp();
    assert_eq!(cap.log_days, LOG_RETENTION_DAYS);
    let start = t0();
    assert!(expired(
        RetentionKind::Log,
        start,
        start + days(LOG_RETENTION_DAYS),
        cap
    ));
}

#[test]
fn delete_does_not_clear_safety_reports() {
    let deleted = apply_local_delete();
    assert_eq!(
        deleted,
        LocalDelete {
            diagnostics: true,
            replays: true,
            pending_queue: QueueState::Cleared,
        }
    );
}

#[test]
fn child_can_still_delete() {
    assert!(!may_collect(
        ProfileKind::Child,
        Consent::Granted,
        Purpose::GameplayLogs
    ));
    assert!(apply_local_delete().diagnostics);
}

#[test]
fn deletion_sla_is_thirty_days() {
    let request = DeleteRequest { now: t0() };
    assert_eq!(request.server_deadline(), t0() + days(DELETION_SLA_DAYS));
    let signed = sign_deletion(&request, &Echo);
    assert_eq!(signed.len(), 8);
}
