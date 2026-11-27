#![cfg(feature = "schedule-shuffle")]

use bevy::{
    ecs::schedule::{LogLevel, ScheduleLabel},
    prelude::*,
};
use harbor_bevy_headless::{
    HeadlessApp, HeadlessConfig, HeadlessGame, ScheduleShuffleConfig, TraceShuffleConfig,
    capture_schedule_order, parse_schedule_seeds,
};

#[derive(Resource)]
struct Value(u64);

struct Game<const ORDERED: bool, const PREINITIALIZE: bool = false>;

impl<const ORDERED: bool, const PREINITIALIZE: bool> HeadlessGame for Game<ORDERED, PREINITIALIZE> {
    type Action = ();
    type Outcome = ();

    fn build(app: &mut App, config: &HeadlessConfig) {
        app.insert_resource(Value(config.seed))
            .add_systems(Update, |mut value: ResMut<Value>| value.0 += 10);
        let double = |mut value: ResMut<Value>| value.0 *= 2;
        let increment = |mut value: ResMut<Value>| value.0 += 1;
        if ORDERED {
            app.add_systems(FixedUpdate, (double, increment).chain());
        } else {
            app.add_systems(FixedUpdate, (double, increment));
        }
        if PREINITIALIZE {
            app.world_mut()
                .schedule_scope(FixedUpdate, |world, schedule| {
                    assert!(schedule.initialize(world).unwrap().is_some());
                    assert!(!schedule.is_changed());
                });
        }
    }

    fn apply(_: &mut HeadlessApp, (): ()) -> anyhow::Result<()> {
        Ok(())
    }

    fn outcome(_: &World) -> Option<()> {
        None
    }
}

fn config() -> ScheduleShuffleConfig {
    ScheduleShuffleConfig {
        headless: HeadlessConfig {
            seed: 5,
            ..Default::default()
        },
        frames: 2,
        // With Bevy 0.20.1, 0..20 exercises both orders of the two writers.
        // Check both hashes rather than coupling the test to any one seed's order.
        seeds: (0..20).collect(),
        ..Default::default()
    }
}

fn state_hash(world: &World) -> u64 {
    world.resource::<Value>().0
}

#[test]
fn ordered_game_matches_baseline_for_twenty_seeds() -> anyhow::Result<()> {
    let report = config().run_game::<Game<true>>(state_hash)?;
    assert_eq!(report.schedules, ["FixedUpdate", "Update"]);
    assert_eq!(report.baseline_hash, 53);
    assert_eq!(report.cases.len(), 20);
    for (seed, case) in (0..20).zip(&report.cases) {
        assert_eq!(case.seed, seed);
        assert_eq!(case.hash, 53);
        assert!(!case.diverged);
    }
    assert!(report.axis_report().passed());
    Ok(())
}

#[test]
fn unordered_game_exposes_both_orders_and_marks_divergences() -> anyhow::Result<()> {
    let mut config = config();
    config.schedules = vec![FixedUpdate.intern()];
    let report = config.run_game::<Game<false>>(state_hash)?;
    assert_eq!(report.schedules, ["FixedUpdate"]);
    assert!(report.cases.iter().any(|case| case.hash == 53));
    assert!(report.cases.iter().any(|case| case.hash == 56));
    assert!(report.cases.iter().any(|case| case.diverged));
    assert!(
        report
            .cases
            .iter()
            .all(|case| case.diverged == (case.hash != report.baseline_hash))
    );
    assert!(!report.axis_report().passed());
    Ok(())
}

#[test]
fn preinitialized_schedule_is_shuffled() -> anyhow::Result<()> {
    let report = config().run_game::<Game<false, true>>(state_hash)?;
    assert!(report.cases.iter().any(|case| case.hash == 53));
    assert!(report.cases.iter().any(|case| case.hash == 56));
    Ok(())
}

#[test]
#[should_panic(
    expected = "pairs of systems with conflicting data access have indeterminate execution order"
)]
fn preinitialized_schedule_reports_ambiguities() {
    let config = ScheduleShuffleConfig {
        ambiguity_detection: Some(LogLevel::Error),
        ..config()
    };
    config.run_game::<Game<false, true>>(state_hash).unwrap();
}

#[test]
fn parses_unique_seeds_without_reordering() {
    assert_eq!(parse_schedule_seeds(" 9, 0, 5 ", 3).unwrap(), [9, 0, 5]);
    for invalid in [
        "",
        " ",
        "1,",
        "1,,2",
        "1,word",
        "1,-2",
        "1,18446744073709551616",
    ] {
        assert!(parse_schedule_seeds(invalid, 0).is_err(), "{invalid}");
    }
    assert!(
        parse_schedule_seeds("1,01", 2)
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    assert!(
        parse_schedule_seeds("1,2", 3)
            .unwrap_err()
            .to_string()
            .contains("at least 3")
    );
}

#[derive(Resource, Default)]
struct Trigger(bool);

struct TracedGame<const ORDERED: bool, const INVARIANT: bool, const SKIP: bool = false>;

impl<const ORDERED: bool, const INVARIANT: bool, const SKIP: bool> HeadlessGame
    for TracedGame<ORDERED, INVARIANT, SKIP>
{
    type Action = bool;
    type Outcome = ();

    fn build(app: &mut App, _: &HeadlessConfig) {
        app.insert_resource(Value(0)).init_resource::<Trigger>();
        // All runs agree until the action at tick 2; tick 3 reconverges.
        let multiply = |trigger: Res<Trigger>, mut value: ResMut<Value>| {
            if trigger.0 {
                if INVARIANT {
                    value.0 += 2;
                } else {
                    value.0 *= 2;
                }
            } else {
                value.0 = 0;
            }
        };
        let increment = |trigger: Res<Trigger>, mut value: ResMut<Value>| {
            if trigger.0 {
                value.0 += 1;
            }
        };
        if ORDERED {
            app.add_systems(FixedUpdate, (multiply, increment).chain());
        } else {
            app.add_systems(FixedUpdate, (multiply.run_if(|| !SKIP), increment));
        }
    }

    fn apply(app: &mut HeadlessApp, active: bool) -> anyhow::Result<()> {
        app.resource_mut::<Trigger>().unwrap().0 = active;
        Ok(())
    }

    fn outcome(_: &World) -> Option<()> {
        None
    }
}

fn trace_config() -> TraceShuffleConfig {
    TraceShuffleConfig {
        schedules: vec![FixedUpdate.intern()],
        require_order_variance: true,
        ..Default::default()
    }
}

#[test]
fn traced_shuffle_finds_first_transient_divergence() -> anyhow::Result<()> {
    let report = trace_config()
        .run_traced::<TracedGame<false, false>, _>(&[false, true, false], state_hash)?;
    assert_eq!(report.first_divergent_tick(), Some(2));
    assert_eq!(report.baseline.digests.len(), 3);
    assert!(report.baseline.orders[0][0].ran.len() >= 2);
    assert!(
        report
            .cases
            .iter()
            .all(|case| case.digests[2] == report.baseline.digests[2])
    );
    assert!(report.variance[0].distinct_built > 1);
    assert!(report.variance[0].distinct_ran > 1);
    Ok(())
}

#[test]
fn traced_shuffle_rejects_vacuous_orders_but_accepts_varied_invariant_runs() -> anyhow::Result<()> {
    let config = trace_config();
    let error = config
        .run_traced::<TracedGame<true, false>, _>(&[true], state_hash)
        .unwrap_err();
    assert!(error.to_string().contains("distinct built 1"), "{error:#}");
    let error = config
        .run_traced::<TracedGame<false, false, true>, _>(&[true], state_hash)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("distinct built 2, distinct ran 1"),
        "{error:#}"
    );
    let report = config.run_traced::<TracedGame<false, true>, _>(&[true, false], state_hash)?;
    assert_eq!(report.first_divergent_tick(), None);
    assert_eq!(report.baseline.digests, [3, 0]);
    Ok(())
}

#[test]
fn order_sample_excludes_systems_skipped_in_the_window() -> anyhow::Result<()> {
    let mut app = HeadlessApp::default();
    app.app_mut()
        .add_systems(Update, (|| {}, (|| {}).run_if(|| false)));
    let before = app.world().read_change_tick();
    app.update();
    let sample = capture_schedule_order(app.world(), Update, before)?;
    assert_eq!(sample.built.len(), 2);
    assert_eq!(sample.ran.len(), 1);
    let sample = capture_schedule_order(app.world(), Update, app.world().read_change_tick())?;
    assert!(sample.ran.is_empty());
    Ok(())
}
