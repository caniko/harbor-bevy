use std::time::Duration;

use bevy::{prelude::*, time::Real};
use harbor_bevy_headless::{CadenceAudit, HeadlessApp, HeadlessConfig, HeadlessGame};

const HALF: Duration = Duration::from_millis(5);
const STEP: Duration = Duration::from_millis(10);
const DOUBLE: Duration = Duration::from_millis(20);

#[derive(Resource, Default)]
struct State(u64);

struct Game<const BUG: bool>;

impl<const BUG: bool> HeadlessGame for Game<BUG> {
    type Action = ();
    type Outcome = ();

    fn build(app: &mut App, _: &HeadlessConfig) {
        app.init_resource::<State>().add_systems(
            FixedLast,
            |mut state: ResMut<State>, real: Res<Time<Real>>| {
                state.0 += if BUG {
                    real.delta().as_millis() as u64
                } else {
                    1
                };
            },
        );
    }

    fn apply(_: &mut HeadlessApp, (): ()) -> anyhow::Result<()> {
        Ok(())
    }

    fn outcome(_: &World) -> Option<()> {
        None
    }
}

fn audit() -> CadenceAudit {
    CadenceAudit {
        headless: HeadlessConfig {
            fixed_dt: STEP,
            ..Default::default()
        },
    }
}

fn digest(world: &World) -> u64 {
    world.resource::<State>().0
}

#[test]
fn samples_every_fixed_tick_even_in_multi_tick_frames() -> anyhow::Result<()> {
    let result =
        audit().run::<Game<false>, _>(&[&[STEP, STEP, STEP], &[HALF, DOUBLE, HALF]], digest)?;
    assert_eq!(result, [1, 2, 3]);
    Ok(())
}

#[test]
fn detects_render_time_leaking_into_fixed_simulation() {
    let error = audit()
        .run::<Game<true>, _>(&[&[STEP, STEP, STEP], &[STEP, DOUBLE]], digest)
        .unwrap_err();
    assert!(error.to_string().contains("fixed tick 2"), "{error:#}");
}

#[test]
fn rejects_vacuous_or_unmatched_cadences() {
    for cadences in [
        &[&[STEP][..]][..],
        &[&[STEP][..], &[STEP][..]],
        &[&[STEP][..], &[DOUBLE][..]],
        &[&[HALF][..], &[Duration::ZERO, HALF][..]],
    ] {
        assert!(audit().run::<Game<false>, _>(cadences, digest).is_err());
    }
}
