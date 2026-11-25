use harbor_bevy_diagnostics::batch_report::{
    DiagnosticReport, FailureCategory, LogRingBuffer, StateDiffReport, ViolationSink,
};
use harbor_bevy_diagnostics::event_audit::{EventAuditConfig, EventAuditState};
use harbor_bevy_diagnostics::log_capture::SessionLogSink;
use harbor_bevy_diagnostics::metrics::SimulationMetrics;
use harbor_bevy_diagnostics::panic_hook::{CrashContext, PanicHookConfig, install_panic_hook};
use harbor_bevy_diagnostics::state_hashing::{
    HashEntry, HashedState, StateHashLog, StateHasher, hash_state_value,
};
use harbor_bevy_diagnostics::{LogCollector, LogLevel, capture_log, clear_remote_spool, spool_dir};
use serde_json::{Value, json};
use tracing_subscriber::prelude::*;

#[test]
fn capture_macro_and_ring_preserve_metadata_and_order() {
    let mut collector = LogCollector::new();
    capture_log!(collector, WARN, "queue {}", 7);
    let entry = collector.take().unwrap().pop().unwrap();
    assert_eq!(entry.level, LogLevel::Warn);
    assert_eq!(entry.target, module_path!());
    assert_eq!(entry.message, "queue 7");
    assert!(chrono::DateTime::parse_from_rfc3339(&entry.timestamp).is_ok());

    let mut disabled = LogRingBuffer::new(0);
    disabled.push(entry.clone());
    assert!(disabled.is_empty());
    let mut ring = LogRingBuffer::new(2);
    ring.push(entry);
    ring.log("INFO", "test", "second".into());
    ring.log("DEBUG", "test", "third".into());
    let messages: Vec<_> = ring.snapshot().into_iter().map(|e| e.message).collect();
    assert_eq!(messages, ["second", "third"]);
}

#[test]
fn diff_distinguishes_absent_fields_from_null_and_sink_drains_shared_reports() {
    let diff = StateDiffReport::from_snapshots(
        json!({"same": 1, "changed": [1], "removed": null})
            .as_object()
            .unwrap(),
        json!({"same": 1, "changed": [2], "added": null})
            .as_object()
            .unwrap(),
    );
    assert!(diff.has_diffs());
    let fields: Vec<_> = diff
        .entries
        .iter()
        .map(|entry| entry.field.as_str())
        .collect();
    assert_eq!(fields, ["added", "changed", "removed"]);
    assert_eq!(diff.entries[0].left_value, None);
    assert_eq!(diff.entries[0].right_value, Some(Value::Null));
    assert_eq!(diff.entries[2].left_value, Some(Value::Null));
    assert!(diff.to_text().contains("<missing>"));

    let sink = ViolationSink::new();
    sink.clone().push(DiagnosticReport {
        session_id: 42,
        failure_category: FailureCategory::Desync,
        state_diff: Some(diff),
        ..Default::default()
    });
    let report = sink.clone().into_reports().pop().unwrap();
    let json: Value = serde_json::from_str(&report.to_json()).unwrap();
    assert_eq!(json["session_id"], 42);
    assert_eq!(json["failure_category"], "Desync");
    assert!(sink.is_empty());
}

#[test]
fn tracing_layer_captures_recent_entries_and_appends_all_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let sink = SessionLogSink::new(dir.path().join("runtime.log"), 1).unwrap();
    let subscriber = tracing_subscriber::registry().with(sink.layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(target: "sample", "first");
        tracing::warn!(target: "sample", "line\nbreak");
    });
    let entries = sink.buffer().entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message, "line\nbreak");
    let on_disk = std::fs::read_to_string(sink.path()).unwrap();
    assert!(on_disk.contains("[INFO] sample: first"));
    assert!(on_disk.contains(r"[WARN] sample: line\nbreak"));
    assert_eq!(on_disk.lines().count(), 2);
}

#[test]
fn metrics_keep_extra_data_separate_from_counters_and_histograms() {
    let mut metrics = SimulationMetrics::new("sample\"id");
    metrics.inc("retries");
    metrics.add("retries", 2);
    metrics.record_histogram("duration", 10);
    metrics.record_histogram("duration", 20);
    let value: Value =
        serde_json::from_str(&metrics.to_json_with_extra(json!({"counters": "metadata"}))).unwrap();
    assert_eq!(value["instance_id"], "sample\"id");
    assert_eq!(value["counters"]["retries"], 3);
    assert_eq!(
        value["histograms"]["duration"],
        json!({"min": 10, "max": 20, "mean": 15.0, "count": 2})
    );
    assert_eq!(value["extra"]["counters"], "metadata");
}

#[test]
fn event_audit_rearms_after_a_quiet_frame() {
    let config = EventAuditConfig {
        per_type_threshold: 1,
        total_threshold: 4,
        sustained_frames: 2,
    };
    let mut audit = EventAuditState::default();
    for tick in 1..=6 {
        if tick != 4 {
            audit.record_batch("sample", 2);
        }
        audit.finish_frame(&config, tick);
    }
    let ticks: Vec<_> = audit.alerts.iter().map(|alert| alert.tick).collect();
    assert_eq!(ticks, [2, 6]);
    let value: Value = serde_json::from_str(&audit.to_json()).unwrap();
    assert_eq!(value["event_types"][0]["lifetime_total"], 10);
    assert_eq!(value["frame_total"], 0);
}

struct Sample(u32);

impl HashedState for Sample {
    fn hash_state(&self, hasher: &mut StateHasher) {
        hasher.update(self.0.to_le_bytes());
    }
}

#[test]
fn caller_state_has_stable_content_hashes() {
    assert_eq!(hash_state_value(&Sample(21)), hash_state_value(&Sample(21)));
    assert_ne!(hash_state_value(&Sample(21)), hash_state_value(&Sample(22)));
}

#[test]
fn hash_log_evicts_old_entries_and_diffs_only_common_ticks() {
    fn entry(tick: u64, value: u32) -> HashEntry {
        HashEntry {
            tick,
            hash: hash_state_value(&Sample(value)),
            component_count: 1,
        }
    }
    let mut left = StateHashLog::default();
    let mut right = StateHashLog::default();
    for tick in 1..=3 {
        left.push_entry(entry(tick, 1), 2);
    }
    right.push_entry(entry(1, 2), 0);
    right.push_entry(entry(2, 1), 0);
    right.push_entry(entry(3, 2), 0);
    assert_eq!(left.entries()[0].tick, 2);
    assert_eq!(StateHashLog::diff(&left, &right), [3]);
}

#[test]
fn crash_bundle_flushes_only_on_configured_tick() {
    use harbor_bevy_diagnostics::crash_dump::{CrashDumpConfig, CrashDumpSession, RuntimeSnapshot};
    let dir = tempfile::tempdir().unwrap();
    let session = CrashDumpSession::new(CrashDumpConfig {
        app_name: "Example Runtime".into(),
        base_dir: dir.path().to_owned(),
        snapshot_flush_interval_ticks: 2,
        ..Default::default()
    })
    .unwrap();
    let bundle = session.session_dir();
    let snapshot = |text: &str| RuntimeSnapshot {
        state_snapshot: text.into(),
        ..Default::default()
    };
    session
        .update_runtime_snapshot(snapshot("first"), 1)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(bundle.join("state_snapshot.txt")).unwrap(),
        ""
    );
    session
        .update_runtime_snapshot(snapshot("second"), 2)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(bundle.join("state_snapshot.txt")).unwrap(),
        "second"
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(bundle.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["app_name"], "Example Runtime");
    assert_eq!(manifest["files"]["app_log"], "app.log");
}

#[test]
fn panic_hook_writes_once_and_uses_caller_banner() {
    let dir = tempfile::tempdir().unwrap();
    // Hooks are process-global. Exercise installation in a separate test process.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "panic_hook_child", "--nocapture"])
        .env("HARBOR_DIAGNOSTICS_TEST_OUTPUT", dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("previous hook"));
    let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(files.len(), 1);
    let text = std::fs::read_to_string(files.into_iter().next().unwrap().unwrap().path()).unwrap();
    assert!(text.contains("Custom banner"));
    assert!(text.contains("Example Runtime"));
    assert!(text.contains("state at failure"));
    assert!(text.contains("first panic"));
    assert!(!text.contains("second panic"));
}

#[test]
fn panic_hook_child() {
    let Some(output_dir) = std::env::var_os("HARBOR_DIAGNOSTICS_TEST_OUTPUT") else {
        return;
    };
    std::panic::set_hook(Box::new(|_| eprintln!("previous hook")));
    let ctx = CrashContext::new("instance", output_dir);
    ctx.set_tick(42);
    ctx.set_state_snapshot("state at failure");
    install_panic_hook(
        &PanicHookConfig {
            app_name: "Example Runtime".into(),
            banner: "Custom banner".into(),
        },
        ctx,
    );
    assert!(std::panic::catch_unwind(|| panic!("first panic")).is_err());
    assert!(std::panic::catch_unwind(|| panic!("second panic")).is_err());
}

#[test]
fn spool_clear_is_idempotent_and_leaves_sibling_data_intact() {
    let dir = tempfile::tempdir().unwrap();
    let spool = spool_dir(dir.path());
    std::fs::create_dir(&spool).unwrap();
    std::fs::write(spool.join("outbox"), "queued").unwrap();
    std::fs::write(dir.path().join("keep"), "unrelated").unwrap();
    clear_remote_spool(dir.path()).unwrap();
    clear_remote_spool(dir.path()).unwrap();
    assert!(!spool.exists());
    assert!(dir.path().join("keep").is_file());
}
