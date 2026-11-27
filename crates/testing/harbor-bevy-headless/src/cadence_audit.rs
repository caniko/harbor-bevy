//! Compare committed fixed-tick values under different render-frame deltas.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, ensure};
use bevy::{
    app::{AppExit, FixedMainScheduleOrder},
    ecs::schedule::ScheduleLabel,
    prelude::{Resource, World},
};

use crate::{HeadlessApp, HeadlessConfig, HeadlessGame};

/// A fresh app is built for each render cadence, always with the same game seed.
#[derive(Clone, Debug, Default)]
pub struct CadenceAudit {
    pub headless: HeadlessConfig,
}

#[derive(Resource)]
struct FixedSamples<T>(Vec<T>);

#[derive(ScheduleLabel, Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CaptureFixedTick;

impl CadenceAudit {
    /// Run distinct frame-delta sequences with equal total duration and compare
    /// their entire fixed-tick traces. `digest` runs once per fixed tick in a
    /// probe appended to the schedule order after game setup.
    /// If the game needs inputs, arrange them in its setup or fixed systems.
    ///
    /// Returns the baseline trace, excluding initial state. Returns an error
    /// for invalid or vacuous cadences, an incomplete run, or the first
    /// differing fixed tick (1-based).
    pub fn run<G: HeadlessGame, D: Eq + Send + Sync + 'static>(
        &self,
        cadences: &[&[Duration]],
        digest: impl Fn(&World) -> D + Send + Sync + 'static,
    ) -> anyhow::Result<Vec<D>> {
        ensure!(
            !self.headless.fixed_dt.is_zero(),
            "fixed timestep must be nonzero"
        );
        let (&first, rest) = cadences.split_first().context("no render cadences")?;
        ensure!(
            rest.iter().any(|&cadence| cadence != first),
            "need distinct render cadences"
        );
        let total = |cadence: &[Duration]| {
            cadence
                .iter()
                .try_fold(Duration::ZERO, |sum, &delta| sum.checked_add(delta))
                .context("render cadence duration overflow")
        };
        let duration = total(first)?;
        for cadence in rest {
            ensure!(
                total(cadence)? == duration,
                "render cadences must cover equal durations"
            );
        }

        let digest = Arc::new(digest);
        let run = |cadence: &[Duration]| {
            let mut app = HeadlessApp::for_game::<G>(self.headless.clone());
            app.finish_plugins();
            app.world_mut()
                .resource_mut::<FixedMainScheduleOrder>()
                .labels
                .push(CaptureFixedTick.intern());
            let digest = Arc::clone(&digest);
            app.app_mut()
                .insert_resource(FixedSamples::<D>(Vec::new()))
                .add_systems(CaptureFixedTick, move |world: &mut World| {
                    let value = digest(world);
                    world.resource_mut::<FixedSamples<D>>().0.push(value);
                });
            for &delta in cadence {
                ensure!(app.can_step(), "app stopped before the cadence ended");
                if let Some(AppExit::Error(code)) = app.update_with_delta(delta) {
                    anyhow::bail!("app exited with error code {code}");
                }
            }
            Ok(std::mem::take(
                &mut app.world_mut().resource_mut::<FixedSamples<D>>().0,
            ))
        };
        let baseline = run(first).context("baseline render cadence")?;
        ensure!(
            !baseline.is_empty(),
            "baseline render cadence produced no fixed ticks"
        );
        for (i, cadence) in rest.iter().enumerate() {
            let index = i + 2;
            let samples = run(cadence).with_context(|| format!("render cadence {index}"))?;
            if let Some(tick) = crate::first_differing_tick(&baseline, &samples) {
                anyhow::bail!("render cadence {index} diverged at fixed tick {tick}");
            }
        }
        Ok(baseline)
    }
}
