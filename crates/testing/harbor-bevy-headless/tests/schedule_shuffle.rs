#![cfg(feature = "schedule-shuffle")]

use bevy::{
    ecs::schedule::{LogLevel, ScheduleLabel},
    prelude::*,
};
use harbor_bevy_headless::{HeadlessApp, HeadlessConfig, HeadlessGame, ScheduleShuffleConfig};

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
