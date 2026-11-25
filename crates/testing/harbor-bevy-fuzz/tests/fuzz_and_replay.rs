use bevy::{app::AppExit, prelude::*};
use harbor_bevy_fuzz::{
    CorpusEntry, CorpusStore, FuzzConfig, FuzzFailure, FuzzModel, TraceConfig, Violation, replay,
    replay_file, replay_hash, run_fuzz,
};
use harbor_bevy_headless::{
    FaultAction, FaultInjectionEnvelope, FaultInjector, HeadlessApp, HeadlessConfig, HeadlessGame,
};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Action {
    Add(u64),
    Rollback,
    Exit,
    Panic,
    Reject,
}

#[derive(Resource)]
struct Counter {
    value: u64,
    timed_fault: bool,
}

struct Toy<const BUG: bool>;

impl<const BUG: bool> HeadlessGame for Toy<BUG> {
    type Action = Action;
    type Outcome = u64;

    fn build(app: &mut App, config: &HeadlessConfig) {
        let seed = config.seed;
        app.add_systems(Startup, move |mut commands: Commands| {
            commands.insert_resource(Counter {
                value: seed,
                timed_fault: false,
            });
        })
        .add_systems(
            Update,
            |mut counter: ResMut<Counter>, injector: Option<Res<FaultInjector>>| {
                if injector.is_some_and(|injector| injector.probe("toy.tick").is_err()) {
                    counter.timed_fault = true;
                }
            },
        );
    }

    fn apply(app: &mut HeadlessApp, action: Action) -> anyhow::Result<()> {
        match action {
            Action::Add(delta) => app.resource_mut::<Counter>().unwrap().value += delta,
            Action::Rollback => app.resource_mut::<Counter>().unwrap().value = 99,
            Action::Exit => {
                app.write_message(AppExit::error());
            }
            Action::Panic => panic!("toy panic"),
            Action::Reject => anyhow::bail!("toy apply error"),
        }
        Ok(())
    }

    fn outcome(world: &World) -> Option<u64> {
        let value = world.resource::<Counter>().value;
        (value >= 30).then_some(value)
    }
}

impl<const BUG: bool> FuzzModel for Toy<BUG> {
    fn strategy(app: &HeadlessApp) -> BoxedStrategy<Action> {
        let value = app.resource::<Counter>().unwrap().value;
        if BUG && value >= 10 {
            Just(Action::Rollback).boxed()
        } else {
            (1..=3u64)
                .prop_map(move |offset| Action::Add(offset + value % 3))
                .boxed()
        }
    }

    fn invariants(app: &HeadlessApp) -> Result<(), Violation> {
        let counter = app.resource::<Counter>().unwrap();
        if counter.value == 99 || counter.timed_fault {
            let mut violation =
                Violation::new("counter.out-of-range", "counter corrupted", app.frames());
            violation
                .context
                .insert("value".to_owned(), counter.value.into());
            return Err(violation);
        }
        Ok(())
    }
}

fn entry(actions: Vec<Action>) -> CorpusEntry<Action> {
    CorpusEntry {
        name: "counter".into(),
        seed: 17,
        config: TraceConfig {
            frames_per_action: 2,
            ..TraceConfig::default()
        },
        fault_envelope: None,
        actions,
    }
}

#[test]
fn multi_case_fuzz_passes() {
    let report = run_fuzz::<Toy<false>>(&FuzzConfig {
        cases: 12,
        max_steps: 8,
        seed: 7,
        save_failures: false,
        ..FuzzConfig::default()
    })
    .unwrap();
    assert_eq!(report.cases_passed, 12);
    assert_eq!(report.failure, None);
    assert_eq!(report.reproducer, None);
}

#[test]
fn corpus_hash_and_file_replay_match() {
    let dir = tempfile::tempdir().unwrap();
    let store = CorpusStore::new(dir.path());
    let trace = entry(vec![Action::Add(2), Action::Add(11)]);
    let path = store.save(&trace).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let hex = blake3::hash(&bytes).to_hex();
    let hash = &hex[..16];
    assert_eq!(path.file_name().unwrap(), format!("{hash}.json").as_str());
    assert_eq!(store.list().unwrap(), [hash]);
    assert_eq!(store.load::<Action>(hash).unwrap(), trace);

    let in_memory = replay::<Toy<false>>(trace).unwrap();
    assert_eq!(in_memory.outcome, Some(30));
    assert_eq!(in_memory.actions_applied, 2);
    assert_eq!(in_memory.frames, 3);
    assert_eq!(replay_file::<Toy<false>>(path).unwrap(), in_memory);
    assert_eq!(replay_hash::<Toy<false>>(&store, hash).unwrap(), in_memory);
}

#[test]
fn malformed_replays_are_load_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invalid.json");
    std::fs::write(&path, b"not JSON").unwrap();
    assert!(matches!(
        replay_file::<Toy<false>>(&path),
        Err(FuzzFailure::ReplayLoad { message }) if message.contains("invalid.json")
    ));
    assert!(matches!(
        replay_hash::<Toy<false>>(&CorpusStore::new(dir.path()), "../invalid"),
        Err(FuzzFailure::ReplayLoad { message }) if message.contains("invalid corpus ID")
    ));
}

#[test]
fn state_dependent_failure_is_saved_and_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let config = FuzzConfig {
        cases: 24,
        max_steps: 12,
        seed: 4,
        corpus_dir: dir.path().to_path_buf(),
        save_failures: true,
        ..FuzzConfig::default()
    };
    let report = run_fuzz::<Toy<true>>(&config).unwrap();
    let failure = report.failure.unwrap();
    let FuzzFailure::InvariantViolation(ref violation) = failure else {
        panic!("unexpected failure: {failure}");
    };
    assert_eq!(violation.rule, "counter.out-of-range");
    assert_eq!(violation.context["value"], 99);
    let path = report.reproducer.unwrap().unwrap();
    let store = CorpusStore::new(dir.path());
    let hashes = store.list().unwrap();
    assert_eq!(hashes.len(), 1);
    let saved = store.load::<Action>(&hashes[0]).unwrap();
    assert!(matches!(saved.actions[0], Action::Add(_)));
    assert_eq!(saved.actions.last(), Some(&Action::Rollback));
    assert_eq!(replay_file::<Toy<true>>(&path), Err(failure.clone()));

    let repeated = run_fuzz::<Toy<true>>(&config).unwrap();
    assert_eq!(repeated.failure, Some(failure));
    assert_eq!(repeated.reproducer, Some(Ok(path)));
}

#[test]
fn faults_are_checked_on_the_exact_frame() {
    let mut trace = entry(vec![Action::Add(1)]);
    trace.fault_envelope = Some(FaultInjectionEnvelope::new(
        1,
        "toy.tick",
        FaultAction::Fail("injected".into()),
    ));
    let Err(FuzzFailure::InvariantViolation(violation)) = replay::<Toy<false>>(trace) else {
        panic!("expected the first post-action frame to violate the invariant");
    };
    assert_eq!(violation.frame, 2);
}

#[test]
fn execution_failures_are_distinct() {
    let mut trace = entry(vec![Action::Add(1)]);
    trace.config.frame_budget = Some(2);
    assert!(matches!(
        replay::<Toy<false>>(trace),
        Err(FuzzFailure::NonTermination { frame: 2, .. })
    ));
    assert!(matches!(
        replay::<Toy<false>>(entry(vec![Action::Exit])),
        Err(FuzzFailure::FatalExit { code: 1, frame: 1 })
    ));
    assert!(matches!(
        replay::<Toy<false>>(entry(vec![Action::Panic])),
        Err(FuzzFailure::Panic { frame: 1, .. })
    ));
    assert!(matches!(
        replay::<Toy<false>>(entry(vec![Action::Reject])),
        Err(FuzzFailure::ApplyError { frame: 1, .. })
    ));
}
