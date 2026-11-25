#![cfg(feature = "bevy")]

use bevy::prelude::*;
use harbor_bevy_diagnostics::crash_dump::{
    CrashDumpConfig, CrashDumpPlugin, CrashDumpResource, CrashDumpSession, cleanup_old_crash_dumps,
};
use harbor_bevy_diagnostics::event_audit::{EventAuditConfig, EventAuditState};
use harbor_bevy_diagnostics::log_capture::LogBuffer;
use harbor_bevy_diagnostics::metrics::{SimulationMetrics, counters, histograms};
use harbor_bevy_diagnostics::plugin::{DiagnosticsPlugin, DiagnosticsPluginConfig};
use harbor_bevy_diagnostics::state_hashing::{HashTarget, HashedState, StateHashLog, StateHasher};
use tracing_subscriber::prelude::*;

struct Sample(u8);

impl HashedState for Sample {
    fn hash_state(&self, hasher: &mut StateHasher) {
        hasher.update([self.0]);
    }
}

#[test]
fn plugin_audits_frames_samples_time_and_hashes_targets_without_entity_ids() {
    fn run(targets: [u8; 2]) -> ([u8; 32], [u8; 32]) {
        let mut app = App::new();
        app.add_plugins(bevy::time::TimePlugin)
            .add_plugins(DiagnosticsPlugin::new(DiagnosticsPluginConfig {
                hash_interval: 2,
                event_audit: EventAuditConfig {
                    per_type_threshold: 1,
                    sustained_frames: 2,
                    ..Default::default()
                },
                ..Default::default()
            }))
            .add_systems(Update, |mut audit: ResMut<EventAuditState>| {
                audit.record_batch("sample", 2)
            });

        let first = app
            .world_mut()
            .spawn(HashTarget::new(&Sample(targets[0])))
            .id();
        app.world_mut().spawn(HashTarget::new(&Sample(targets[1])));
        app.update();
        assert!(app.world().resource::<StateHashLog>().entries().is_empty());
        app.update();
        let world = app.world();
        let first_hash = world.resource::<StateHashLog>().entries()[0].hash;
        assert_eq!(world.resource::<EventAuditState>().alerts[0].tick, 2);
        let metrics = world.resource::<SimulationMetrics>();
        assert_eq!(metrics.counters[counters::EVENT_FLOODS_DETECTED], 1);
        assert_eq!(metrics.histograms[histograms::FRAME_TIME_US].count, 2);
        app.world_mut()
            .get_mut::<HashTarget>(first)
            .unwrap()
            .update(&Sample(9));
        app.update();
        app.update();
        let log = app.world().resource::<StateHashLog>();
        assert_eq!(log.entries()[1].tick, 4);
        (first_hash, log.entries()[1].hash)
    }
    let (before, after) = run([1, 2]);
    assert_eq!(before, run([2, 1]).0);
    assert_ne!(before, after);
}

#[derive(Resource)]
struct SnapshotText(&'static str);

#[test]
fn crash_dump_plugin_uses_snapshot_provider_and_fatal_bundle_records_once() {
    let dir = tempfile::tempdir().unwrap();
    let session = CrashDumpSession::new(CrashDumpConfig {
        base_dir: dir.path().to_path_buf(),
        snapshot_flush_interval_ticks: 1,
        ..Default::default()
    })
    .unwrap();
    let mut app = App::new();
    app.insert_resource(SnapshotText("caller-owned state"))
        .add_plugins(DiagnosticsPlugin::default())
        .add_plugins(CrashDumpPlugin::new(session.clone(), |world| {
            world.resource::<SnapshotText>().0.to_owned()
        }));
    app.update();
    let dir = session.session_dir();
    assert_eq!(
        std::fs::read_to_string(dir.join("state_snapshot.txt")).unwrap(),
        "caller-owned state"
    );
    assert!(app.world().contains_resource::<CrashDumpResource>());
    let subscriber = tracing_subscriber::registry().with(session.boxed_log_layer());
    tracing::subscriber::with_default(subscriber, || tracing::info!("bundle test"));
    assert_eq!(
        app.world().resource::<LogBuffer>().entries()[0].message,
        "bundle test"
    );

    #[derive(Debug)]
    struct Fatal(u8);
    impl std::fmt::Display for Fatal {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Example exit {}", self.0)
        }
    }
    impl harbor_bevy_core::FatalCode for Fatal {
        fn exit_code(&self) -> u8 {
            self.0
        }
        fn label(&self) -> &str {
            "example_failure"
        }
    }
    app.world_mut()
        .trigger(harbor_bevy_core::GracefulShutdownRequest::new(Fatal(23)));
    session.record_fatal_code(&Fatal(24)).unwrap();
    let fatal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("fatal_error.json")).unwrap())
            .unwrap();
    assert_eq!(fatal["exit_code"], 23);
    assert_eq!(fatal["label"], "example_failure");
    assert_eq!(fatal["message"], "Example exit 23");
}

#[test]
fn crash_dump_rejects_invalid_ids_and_retention_ignores_unrelated_directories() {
    use std::time::{Duration, SystemTime};
    let root = tempfile::tempdir().unwrap();
    let config = CrashDumpConfig {
        base_dir: root.path().to_path_buf(),
        ..Default::default()
    };
    let unrelated = root.path().join("unrelated");
    std::fs::create_dir(&unrelated).unwrap();
    assert!(
        CrashDumpSession::new(CrashDumpConfig {
            instance_id: "../escape".into(),
            ..config.clone()
        })
        .is_err()
    );
    let session = CrashDumpSession::new(config.clone()).unwrap();
    assert!(CrashDumpSession::new(config).is_err());
    let old = SystemTime::now() - Duration::from_secs(3 * 86_400);
    for dir in [session.session_dir(), unrelated.as_path()] {
        std::fs::File::open(dir).unwrap().set_modified(old).unwrap();
    }
    cleanup_old_crash_dumps(root.path(), 2).unwrap();
    assert!(!session.session_dir().exists());
    assert!(unrelated.is_dir());
}
