use std::collections::HashSet;

use anyhow::{Context, ensure};
use bevy::{
    app::{AppExit, FixedUpdate, Update},
    ecs::{
        change_detection::Tick,
        schedule::{InternedScheduleLabel, LogLevel, ScheduleLabel, Schedules, SystemSet},
        world::World,
    },
    time::{Fixed, Time},
};
use serde::{Deserialize, Serialize};

use crate::{
    AxisReport, HeadlessApp, HeadlessConfig, HeadlessGame, ScenarioOutcome, ScenarioResult,
};

#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RebuildSchedule;

fn prepare_run<G: HeadlessGame>(
    config: &HeadlessConfig,
    schedules: &[InternedScheduleLabel],
    ambiguity_detection: Option<LogLevel>,
    seed: Option<u64>,
) -> HeadlessApp {
    let mut app = HeadlessApp::for_game::<G>(config.clone());
    app.finish_plugins();
    for &label in schedules {
        app.app_mut().edit_schedule(label, |schedule| {
            let mut settings = schedule.get_build_settings();
            settings.shuffle_seed = seed;
            if let Some(level) = ambiguity_detection {
                settings.ambiguity_detection = level;
            }
            schedule.set_build_settings(settings);
            if !schedule.is_changed() && schedule.systems_len() != 0 {
                // Bevy does not invalidate the graph when build settings change.
                schedule.configure_sets(RebuildSchedule);
            }
        });
    }
    app
}

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
            let mut app = prepare_run::<G>(
                &self.headless,
                &self.schedules,
                self.ambiguity_detection,
                seed,
            );
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

/// Parse comma-separated decimal `u64` seeds, preserving their order and
/// trimming surrounding whitespace from each entry.
///
/// Returns an error for empty or malformed entries, duplicate values, or fewer
/// than `min_count` entries.
pub fn parse_schedule_seeds(value: &str, min_count: usize) -> anyhow::Result<Vec<u64>> {
    let mut seen = HashSet::new();
    let seeds = value
        .split(',')
        .enumerate()
        .map(|(i, field)| {
            let seed = field
                .trim()
                .parse()
                .with_context(|| format!("invalid seed at entry {}", i + 1))?;
            ensure!(seen.insert(seed), "duplicate schedule seed {seed}");
            Ok(seed)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(
        seeds.len() >= min_count,
        "expected at least {min_count} seeds, got {}",
        seeds.len()
    );
    Ok(seeds)
}

/// System identities as debug signatures, not stable identifiers across builds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleOrderSample {
    /// Systems in the built schedule's order, whether or not they ran.
    pub built: Vec<String>,
    /// Observed order by latest execution change tick. Not a happens-before
    /// guarantee for systems that can run simultaneously.
    pub ran: Vec<String>,
}

/// Inspect a built schedule after a bounded observation window. Pass
/// [`World::read_change_tick`] taken immediately before stepping. A system
/// that ran multiple times in the window appears once, at its last run.
///
/// Returns an error if the schedule is missing or has not been initialized.
pub fn capture_schedule_order(
    world: &World,
    label: impl ScheduleLabel,
    before: Tick,
) -> anyhow::Result<ScheduleOrderSample> {
    let schedule = world
        .get_resource::<Schedules>()
        .and_then(|schedules| schedules.get(label))
        .context("schedule missing after update")?;
    let start = before.get();
    let width = world.read_change_tick().get().wrapping_sub(start);
    let mut ran = Vec::new();
    let built = schedule
        .systems()?
        .map(|(key, system)| {
            let signature = format!("{} ({key:?})", system.name());
            let offset = system.get_last_run().get().wrapping_sub(start);
            if offset < width {
                ran.push((offset, signature.clone()));
            }
            signature
        })
        .collect();
    ran.sort_unstable();
    Ok(ScheduleOrderSample {
        built,
        ran: ran.into_iter().map(|(_, name)| name).collect(),
    })
}

/// Replay the same actions with and without schedule shuffling.
#[derive(Clone, Debug)]
pub struct TraceShuffleConfig {
    pub headless: HeadlessConfig,
    pub schedules: Vec<InternedScheduleLabel>,
    pub seeds: Vec<u64>,
    pub ambiguity_detection: Option<LogLevel>,
    /// Require variation across seeded runs in both built and observed order
    /// for every selected schedule. The baseline does not count.
    pub require_order_variance: bool,
}

impl Default for TraceShuffleConfig {
    fn default() -> Self {
        let config = ScheduleShuffleConfig::default();
        Self {
            headless: config.headless,
            schedules: config.schedules,
            seeds: config.seeds,
            ambiguity_detection: config.ambiguity_detection,
            require_order_variance: false,
        }
    }
}

impl TraceShuffleConfig {
    /// Apply one action immediately before each frame and read the digest after
    /// the frame. Each frame must advance exactly one fixed timestep. No digest
    /// is taken before the first action.
    ///
    /// Returns an error for invalid input, an incomplete run, an unavailable
    /// schedule sample, or insufficient variation when requested.
    pub fn run_traced<G: HeadlessGame, D: Eq>(
        &self,
        actions: &[G::Action],
        digest: impl Fn(&World) -> D,
    ) -> anyhow::Result<TraceShuffleReport<D>>
    where
        G::Action: Clone,
    {
        ensure!(
            !self.headless.fixed_dt.is_zero(),
            "fixed timestep must be nonzero"
        );
        ensure!(!actions.is_empty(), "no actions to trace");
        ensure!(
            !self.schedules.is_empty(),
            "no schedules selected for shuffling"
        );
        ensure!(!self.seeds.is_empty(), "no schedule seeds");
        ensure!(
            self.seeds.iter().collect::<HashSet<_>>().len() == self.seeds.len(),
            "duplicate schedule seeds"
        );
        let schedules: Vec<_> = self
            .schedules
            .iter()
            .map(|label| format!("{label:?}"))
            .collect();
        let run = |seed| {
            let mut app = prepare_run::<G>(
                &self.headless,
                &self.schedules,
                self.ambiguity_detection,
                seed,
            );
            let mut digests = Vec::with_capacity(actions.len());
            let mut orders = Vec::with_capacity(actions.len());
            for (index, action) in actions.iter().enumerate() {
                let tick = index + 1;
                ensure!(app.can_step(), "app stopped before tick {tick}");
                app.apply_action::<G>(action.clone())
                    .with_context(|| format!("action at tick {tick}"))?;
                let before = app.world().read_change_tick();
                let elapsed = app.world().resource::<Time<Fixed>>().elapsed();
                if let Some(AppExit::Error(code)) = app.update() {
                    anyhow::bail!("app exited with error code {code} at tick {tick}");
                }
                let fixed = app.world().resource::<Time<Fixed>>();
                ensure!(
                    fixed.timestep() == self.headless.fixed_dt
                        && fixed.elapsed().checked_sub(elapsed) == Some(self.headless.fixed_dt),
                    "expected one fixed step at tick {tick}"
                );
                digests.push(digest(app.world()));
                orders.push(
                    self.schedules
                        .iter()
                        .map(|&label| {
                            capture_schedule_order(app.world(), label, before)
                                .with_context(|| format!("{label:?} order at tick {tick}"))
                        })
                        .collect::<anyhow::Result<_>>()?,
                );
            }
            Ok(TraceShuffleCase {
                seed,
                digests,
                orders,
            })
        };

        let baseline = run(None).context("unshuffled baseline")?;
        let cases = self
            .seeds
            .iter()
            .map(|&seed| run(Some(seed)).with_context(|| format!("schedule shuffle seed {seed}")))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let variance = schedules.iter().enumerate().map(|(i, label)| {
            let distinct_built = cases.iter().map(|case| {
                case.orders.iter().map(|tick| &tick[i].built).collect::<Vec<_>>()
            }).collect::<HashSet<_>>().len();
            let distinct_ran = cases.iter().filter_map(|case| {
                let signature: Vec<_> = case.orders.iter().map(|tick| &tick[i].ran).collect();
                signature.iter().any(|order| !order.is_empty()).then_some(signature)
            }).collect::<HashSet<_>>().len();
            ensure!(
                !self.require_order_variance || (distinct_built > 1 && distinct_ran > 1),
                "{label} has insufficient order variance (distinct built {distinct_built}, distinct ran {distinct_ran})"
            );
            Ok(ScheduleOrderVariance { distinct_built, distinct_ran })
        }).collect::<anyhow::Result<_>>()?;
        Ok(TraceShuffleReport {
            schedules,
            baseline,
            cases,
            variance,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleOrderVariance {
    pub distinct_built: usize,
    /// Distinct observed traces among runs that executed at least one system
    /// in this schedule. Empty windows within a trace are still retained.
    pub distinct_ran: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceShuffleCase<D> {
    /// `None` for the unshuffled baseline.
    pub seed: Option<u64>,
    pub digests: Vec<D>,
    /// Indexed by tick, then by schedule in report order.
    pub orders: Vec<Vec<ScheduleOrderSample>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceShuffleReport<D> {
    pub schedules: Vec<String>,
    pub baseline: TraceShuffleCase<D>,
    pub cases: Vec<TraceShuffleCase<D>>,
    /// Indexed by schedule in report order.
    pub variance: Vec<ScheduleOrderVariance>,
}

impl<D: PartialEq> TraceShuffleReport<D> {
    /// Earliest divergence from baseline across all seeded runs (1-based).
    pub fn first_divergent_tick(&self) -> Option<usize> {
        self.cases
            .iter()
            .filter_map(|case| crate::first_differing_tick(&self.baseline.digests, &case.digests))
            .min()
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
