use std::time::Duration;

use bevy::{app::AppExit, prelude::*, time::Virtual};
use harbor_bevy_headless::{
    AxisReport, BatchConfig, BatchRunner, BatchSession, FailureCategory, FaultAction,
    FaultInjectionEnvelope, FaultInjector, FaultInjectorPlugin, HeadlessApp, HeadlessConfig,
    HeadlessGame, LogEntry, LogLevel, MatrixReport, PeerCluster, ScenarioOutcome, ScenarioResult,
    SessionBugReport, SessionOutcome, SnapshotField, probe_fault,
};
use serde_json::{Value, json};

#[derive(Resource)]
struct Counter {
    ticks: u64,
    target: u64,
}

#[derive(Event)]
struct Target(u64);

#[derive(States, Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
enum Phase {
    #[default]
    Waiting,
    Running,
}

struct Toy;

impl HeadlessGame for Toy {
    type Action = u64;
    type Outcome = u64;

    fn build(app: &mut App, cfg: &HeadlessConfig) {
        app.init_state::<Phase>()
            .insert_resource(Counter {
                ticks: cfg.seed,
                target: u64::MAX,
            })
            .add_systems(FixedUpdate, |mut count: ResMut<Counter>| {
                count.ticks += 1;
            })
            .add_observer(|target: On<Target>, mut counter: ResMut<Counter>| {
                counter.target = target.0;
            });
    }

    fn apply(app: &mut HeadlessApp, action: u64) -> anyhow::Result<()> {
        anyhow::ensure!(action > 0, "target must be positive");
        app.trigger(Target(action));
        Ok(())
    }

    fn outcome(world: &World) -> Option<u64> {
        let count = world.resource::<Counter>();
        (count.ticks >= count.target).then_some(count.ticks)
    }
}

#[test]
fn manual_time_states_observers_and_budget() {
    let mut app = HeadlessApp::for_game::<Toy>(HeadlessConfig {
        seed: 3,
        fixed_dt: Duration::from_millis(10),
        frame_budget: Some(4),
        ..Default::default()
    });
    assert_eq!(app.state::<Phase>(), Some(&Phase::Waiting));
    assert!(app.run_until(0, |world| world.resource::<Counter>().ticks == 3));
    app.set_next_state(Phase::Running);

    assert_eq!(app.step_action::<Toy>(6).unwrap(), None);
    assert_eq!(app.state::<Phase>(), Some(&Phase::Running));
    assert_eq!(app.resource::<Counter>().unwrap().ticks, 4);
    app.update_with_delta(Duration::from_millis(5));
    assert_eq!(app.resource::<Counter>().unwrap().ticks, 4);
    assert_eq!(app.advance_fixed(2), 2);
    assert_eq!(app.outcome::<Toy>(), Some(6));
    assert_eq!(
        app.resource::<Time<Virtual>>().unwrap().elapsed(),
        Duration::from_millis(35)
    );
    assert_eq!(app.run_frames(10), 0);
    assert_eq!(app.frames(), 4);
    assert!(!app.run_until(1, |_| false));
}

#[test]
fn exit_is_detected_and_stops_stepping() {
    let mut app = HeadlessApp::new(HeadlessConfig::default());
    app.app_mut()
        .add_systems(Update, |mut out: MessageWriter<AppExit>| {
            out.write(AppExit::error());
        });
    assert_eq!(app.run_frames(5), 1);
    assert_eq!(app.app_exit(), Some(AppExit::error()));
    assert_eq!(app.update(), Some(AppExit::error()));
    assert_eq!(app.frames(), 1);

    let mut app = HeadlessApp::default();
    assert!(app.write_message(AppExit::Success).is_some());
    assert!(app.write_message(AppExit::from_code(7)).is_some());
    assert_eq!(app.update(), Some(AppExit::from_code(7)));
    assert_eq!(app.frames(), 0);
}

#[test]
fn seeded_game_batch_and_matrix_snapshot() {
    let runner = BatchRunner::new(BatchConfig {
        session_count: 3,
        max_frames: 3,
        headless: HeadlessConfig {
            seed: 9,
            ..Default::default()
        },
    });
    let run = || {
        runner.run_game::<Toy, _>(|cfg| {
            [match cfg.seed {
                9 => 11,
                10 => 0,
                _ => 999,
            }]
        })
    };
    let report = run();
    assert_eq!(report, run());
    assert_eq!(report.sessions[0].outcome, SessionOutcome::Completed(11));
    assert_eq!(report.sessions[0].steps, 2);
    assert!(matches!(
        report.sessions[1].outcome,
        SessionOutcome::Failed { .. }
    ));
    assert_eq!(report.sessions[1].steps, 1);
    assert_eq!(
        report.sessions[2].outcome,
        SessionOutcome::FrameBudgetExhausted
    );
    assert_eq!(report.sessions[2].steps, 3);

    let mut matrix = MatrixReport::new(["simulation", "recovery"]);
    let scenario = ScenarioResult {
        name: "seed 9".into(),
        outcome: ScenarioOutcome::Passed,
        message: None,
    };
    matrix.set_axis("simulation", AxisReport::new(vec![scenario.clone()]));
    assert!(!matrix.all_passed());
    assert_eq!(
        MatrixReport::from_snapshot_json(&matrix.snapshot_json().unwrap()).unwrap(),
        matrix
    );
    matrix.set_axis("recovery", AxisReport::new(vec![scenario]));
    assert!(matrix.all_passed());
}

#[test]
fn custom_batch_session_respects_smaller_app_budget() {
    struct Session(u64);

    impl BatchSession for Session {
        type Outcome = u64;

        fn step(&mut self) -> anyhow::Result<()> {
            self.0 += 1;
            Ok(())
        }

        fn outcome(&self) -> Option<u64> {
            (self.0 == 2).then_some(self.0)
        }
    }

    let report = BatchRunner::new(BatchConfig {
        headless: HeadlessConfig {
            frame_budget: Some(1),
            ..Default::default()
        },
        ..Default::default()
    })
    .run(|index, cfg| {
        assert_eq!(index, 0);
        assert_eq!(cfg.frame_budget, Some(1));
        Ok(Session(0))
    });
    assert_eq!(
        report.sessions[0].outcome,
        SessionOutcome::FrameBudgetExhausted
    );
    assert_eq!(report.sessions[0].steps, 1);
}

#[test]
fn cluster_steps_indexed_apps_and_holds_delivery_without_pausing_updates() {
    #[derive(Resource, Default)]
    struct Delivery(bool);

    let mut peers = PeerCluster::new((0..3).map(|seed| {
        HeadlessApp::for_game::<Toy>(HeadlessConfig {
            seed,
            ..Default::default()
        })
    }))
    .with_delivery_hook(|_, app, held| app.insert_resource(Delivery(held)));

    peers.step_subset(&[2, 0, 2]).unwrap();
    assert!(peers.step_subset(&[1, 3]).is_err());
    assert_eq!(
        peers
            .peers()
            .iter()
            .map(HeadlessApp::frames)
            .collect::<Vec<_>>(),
        [1, 0, 1]
    );
    assert!(peers.hold_delivery(1).unwrap());
    assert!(!peers.hold_delivery(1).unwrap());
    assert!(peers.peer(1).unwrap().resource::<Delivery>().unwrap().0);
    peers.step_peer(1).unwrap();
    assert!(peers.release_delivery(1).unwrap());
    assert!(!peers.peer(1).unwrap().resource::<Delivery>().unwrap().0);
    peers.run_frames_all(1);
    assert!(peers.run_until(1, |cluster| {
        cluster.peers().iter().all(|app| app.frames() == 3)
    }));
    assert!(!peers.run_until(1, |cluster| cluster.peer(0).unwrap().frames() == 5));
    assert_eq!(peers.peer(0).unwrap().frames(), 4);
}

#[test]
fn scheduled_faults_are_visible_only_in_the_target_frame_and_world() {
    #[derive(Resource, Default)]
    struct Hits(Vec<(u64, Option<FaultAction>)>);

    fn instrumented_peer() -> HeadlessApp {
        let mut app = HeadlessApp::for_game::<Toy>(HeadlessConfig::default());
        app.app_mut()
            .add_plugins(FaultInjectorPlugin)
            .init_resource::<Hits>()
            .add_systems(Update, |world: &mut World| {
                let injector = world.resource::<FaultInjector>();
                let hit = injector.probe("read").err();
                assert_eq!(hit, probe_fault("read").err());
                let frame = injector.frame().unwrap();
                world.resource_mut::<Hits>().0.push((frame, hit));
            });
        app
    }

    let mut peers = PeerCluster::new([instrumented_peer(), instrumented_peer()]);
    let fault = FaultAction::Panic("synthetic panic".into());
    let envelope = FaultInjectionEnvelope::new(1, "read", fault.clone());
    let wire = serde_json::to_string(&envelope).unwrap();
    let envelope = serde_json::from_str(&wire).unwrap();
    peers
        .peer_mut(0)
        .unwrap()
        .resource_mut::<FaultInjector>()
        .unwrap()
        .schedule(envelope)
        .unwrap();
    peers.run_frames_all(3);

    assert_eq!(
        peers.peer(0).unwrap().resource::<Hits>().unwrap().0,
        [(0, None), (1, Some(fault)), (2, None)]
    );
    assert_eq!(
        peers.peer(1).unwrap().resource::<Hits>().unwrap().0,
        [(0, None), (1, None), (2, None)]
    );
    assert_eq!(probe_fault("read"), Ok(()));
}

#[test]
fn bug_report_round_trips_with_missing_and_null_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let mut report = SessionBugReport::new(4, FailureCategory::Desync, 0.125, "peers diverged");
    report.set_snapshots(
        json!({"tick": 3, "shared": 0, "optional": null})
            .as_object()
            .cloned(),
        json!({"tick": 4, "shared": 0}).as_object().cloned(),
    );
    report
        .logs
        .push(LogEntry::new(LogLevel::Error, "sim", "hash mismatch"));
    let path = report.write_to_dir(dir.path()).unwrap();
    assert_eq!(path.file_name().unwrap(), "bug_report.json");
    let parsed: SessionBugReport = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed, report);
    let diff = parsed.state_diff().unwrap();
    assert_eq!(
        diff.keys().map(String::as_str).collect::<Vec<_>>(),
        ["optional", "tick"]
    );
    assert_eq!(diff["optional"].peer_0, SnapshotField::Value(Value::Null));
    assert_eq!(diff["optional"].peer_1, SnapshotField::Missing);

    report.elapsed_secs = f64::NAN;
    assert!(report.write_to_file(&path).is_err());
    assert_eq!(
        parsed,
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
    );
    report.elapsed_secs = 0.25;
    report.write_to_file(&path).unwrap();
    assert_eq!(
        report,
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
    );
    assert_eq!(dir.path().read_dir().unwrap().count(), 1);

    let occupied = dir.path().join("occupied");
    std::fs::create_dir(&occupied).unwrap();
    assert!(report.write_to_file(&occupied).is_err());
    assert!(occupied.is_dir());
    assert_eq!(dir.path().read_dir().unwrap().count(), 2);
}
