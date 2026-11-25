use anyhow::{Context, ensure};
use bevy::{
    app::{AppExit, FixedUpdate, Update},
    ecs::{
        schedule::{InternedScheduleLabel, LogLevel, ScheduleLabel},
        world::World,
    },
};
use serde::{Deserialize, Serialize};

use crate::{
    AxisReport, HeadlessApp, HeadlessConfig, HeadlessGame, ScenarioOutcome, ScenarioResult,
};

/// Compare game state across Bevy schedule build seeds.
#[derive(Clone, Debug)]
pub struct ScheduleShuffleConfig {
    /// Game config, including the same game seed for every run.
    pub headless: HeadlessConfig,
    /// Number of full frames to execute per run using [`HeadlessApp::run_frames`].
    pub frames: u64,
    /// Schedules to shuffle together, defaulting to `FixedUpdate` and `Update`.
    /// The game or Bevy's main loop must actually run these schedules.
    pub schedules: Vec<InternedScheduleLabel>,
    /// Schedule build seeds in execution order, defaulting to `0..20`.
    pub seeds: Vec<u64>,
    /// Override Bevy's ambiguity logging for the selected schedules in all
    /// runs. `None` preserves the game's settings. `Error` can cause a Bevy
    /// panic during schedule initialization, rather than a hash comparison.
    pub ambiguity_detection: Option<LogLevel>,
}

impl Default for ScheduleShuffleConfig {
    fn default() -> Self {
        Self {
            headless: HeadlessConfig::default(),
            frames: 600,
            schedules: vec![FixedUpdate.intern(), Update.intern()],
            seeds: (0..20).collect(),
            ambiguity_detection: None,
        }
    }
}

impl ScheduleShuffleConfig {
    /// Build a fresh app per run without injecting actions. Compare the caller's
    /// final state hash against a run with shuffling disabled on the selected
    /// schedules. Other schedules and the game's executor choices are unchanged.
    ///
    /// Returns an error on a zero frame count, no selected schedules, a run
    /// stopping before the frame count, or an error exit. An empty seed list
    /// produces no cases (a missing matrix axis).
    pub fn run_game<G: HeadlessGame>(
        &self,
        state_hash: impl Fn(&World) -> u64,
    ) -> anyhow::Result<ScheduleShuffleReport> {
        ensure!(
            self.frames > 0,
            "schedule shuffle requires at least one frame"
        );
        ensure!(
            !self.schedules.is_empty(),
            "no schedules selected for shuffling"
        );
        let run = |seed| {
            let mut app = HeadlessApp::for_game::<G>(self.headless.clone());
            app.finish_plugins();
            for &label in &self.schedules {
                app.app_mut().edit_schedule(label, |schedule| {
                    let mut settings = schedule.get_build_settings();
                    settings.shuffle_seed = seed;
                    if let Some(level) = self.ambiguity_detection {
                        settings.ambiguity_detection = level;
                    }
                    schedule.set_build_settings(settings);
                });
            }
            let frames = app.run_frames(self.frames);
            if let Some(AppExit::Error(code)) = app.app_exit() {
                anyhow::bail!("game exited with error code {code} after {frames} frames");
            }
            ensure!(
                frames == self.frames,
                "ran {frames} of {} frames",
                self.frames
            );
            Ok(state_hash(app.world()))
        };

        let baseline_hash = run(None).context("unshuffled baseline")?;
        let cases = self
            .seeds
            .iter()
            .map(|&seed| {
                let hash =
                    run(Some(seed)).with_context(|| format!("schedule shuffle seed {seed}"))?;
                Ok(ScheduleShuffleCase {
                    seed,
                    hash,
                    diverged: hash != baseline_hash,
                })
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(ScheduleShuffleReport {
            schedules: self
                .schedules
                .iter()
                .map(|label| format!("{label:?}"))
                .collect(),
            baseline_hash,
            cases,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleShuffleCase {
    pub seed: u64,
    pub hash: u64,
    pub diverged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleShuffleReport {
    /// Debug names of the selected schedule labels, shared by all cases.
    pub schedules: Vec<String>,
    pub baseline_hash: u64,
    pub cases: Vec<ScheduleShuffleCase>,
}

impl ScheduleShuffleReport {
    /// Summarize this sweep as a pass/fail axis for the test matrix.
    pub fn axis_report(&self) -> AxisReport {
        AxisReport::new(
            self.cases
                .iter()
                .map(|case| ScenarioResult {
                    name: format!("seed {}", case.seed),
                    outcome: if case.diverged {
                        ScenarioOutcome::Failed
                    } else {
                        ScenarioOutcome::Passed
                    },
                    message: case.diverged.then(|| {
                        format!(
                            "hash {} differs from baseline {}",
                            case.hash, self.baseline_hash
                        )
                    }),
                })
                .collect(),
        )
    }
}
