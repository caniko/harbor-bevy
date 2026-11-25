use std::{
    fs,
    num::NonZeroU8,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bevy::{app::AppExit, prelude::*, time::Real};
use harbor_bevy_core::harness::{
    EXIT_CODE_ERROR, EXIT_CODE_SUCCESS, ProgressHeartbeatConfig, ProgressSample,
    cleanup_stale_heartbeat_files, exit_code_to_app_exit, format_exit_code, format_ready_marker,
    write_progress_heartbeat,
};
use serde_json::Value;

#[test]
fn heartbeat_uses_real_time_and_counts_progress_changes() {
    let dir = Scratch::new();
    let path = dir.0.join("game_progress_test.json");
    let mut config = ProgressHeartbeatConfig::new(&path);
    config.interval_secs = 2.0;
    config.trace_writes = true;
    let mut app = App::new();
    app.init_resource::<Time<Real>>()
        .insert_resource(config)
        .insert_resource(ProgressSample {
            phase: "a \"quoted\"\nphase".into(),
            round: 1,
            turn: 2,
            elapsed_phase_secs: 0.5,
        })
        .add_systems(Update, write_progress_heartbeat);

    let tick = |app: &mut App, secs| {
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(Duration::from_secs(secs));
        app.update();
    };
    let read = || serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap();

    tick(&mut app, 1);
    assert!(!path.exists());
    tick(&mut app, 1);
    let first = read();
    assert_eq!(first["phase"], "a \"quoted\"\nphase");
    assert_eq!(first["round"], 1);
    assert_eq!(first["turn"], 2);
    assert_eq!(first["pid"], std::process::id());
    assert_eq!(first["progress_seq"], 0);
    assert_eq!(first["elapsed_phase_secs"], 0.5);
    assert!(first["timestamp_ms"].as_u64().unwrap() > 0);
    assert!(first.get("run_id").is_none());

    app.world_mut()
        .resource_mut::<ProgressSample>()
        .elapsed_phase_secs = 2.5;
    app.world_mut()
        .resource_mut::<ProgressHeartbeatConfig>()
        .run_id = Some("run\"1".into());
    tick(&mut app, 2);
    assert_eq!(read()["progress_seq"], 0);
    assert_eq!(read()["elapsed_phase_secs"], 2.5);
    assert_eq!(read()["run_id"], "run\"1");

    app.world_mut().resource_mut::<ProgressSample>().turn += 1;
    tick(&mut app, 2);
    assert_eq!(read()["progress_seq"], 1);
    assert!(!path.with_extension("json.tmp").exists());
    drop(app);
    assert!(!path.exists());
}

#[test]
fn cleanup_removes_stale_and_malformed_heartbeats_only() {
    let dir = Scratch::new();
    let fresh = dir.0.join("game_progress_fresh.json");
    let stale = dir.0.join("game_progress_stale.json");
    let invalid = dir.0.join("game_progress_invalid.json");
    let unrelated = dir.0.join("unrelated.json");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let pid = std::process::id();
    fs::write(
        &fresh,
        format!(r#"{{"pid":{pid},"timestamp_ms":{timestamp}}}"#),
    )
    .unwrap();
    fs::write(&stale, format!(r#"{{"pid":{pid},"timestamp_ms":1}}"#)).unwrap();
    fs::write(&invalid, b"not a heartbeat").unwrap();
    fs::write(&unrelated, b"not a heartbeat").unwrap();
    for path in [&fresh, &stale, &invalid] {
        fs::write(path.with_extension("json.tmp"), b"partial").unwrap();
    }
    assert_eq!(cleanup_stale_heartbeat_files(&dir.0), 2);
    assert!(!stale.exists());
    assert!(!stale.with_extension("json.tmp").exists());
    assert!(!invalid.exists());
    assert!(!invalid.with_extension("json.tmp").exists());
    assert!(fresh.exists());
    assert!(fresh.with_extension("json.tmp").exists());
    assert!(unrelated.exists());
    assert_eq!(cleanup_stale_heartbeat_files(&dir.0.join("missing")), 0);
}

#[test]
fn harness_markers_and_exit_codes_are_product_neutral() {
    assert_eq!(format_ready_marker("listening"), "[READY] listening");
    assert_eq!(format_ready_marker(""), "[READY]");
    assert_eq!(format_exit_code(EXIT_CODE_SUCCESS), "SUCCESS (0)");
    assert_eq!(format_exit_code(EXIT_CODE_ERROR), "ERROR (1)");
    assert_eq!(format_exit_code(27), "UNKNOWN (27)");
    assert_eq!(exit_code_to_app_exit(EXIT_CODE_SUCCESS), AppExit::Success);
    assert_eq!(
        exit_code_to_app_exit(27),
        AppExit::Error(NonZeroU8::new(27).unwrap())
    );
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("harbor-bevy-core-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
