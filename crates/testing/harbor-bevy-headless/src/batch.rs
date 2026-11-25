//! Run independent sessions sequentially, with reproducible seeds and step limits.

use std::marker::PhantomData;

use bevy::app::AppExit;
use serde::{Deserialize, Serialize};

use crate::{HeadlessApp, HeadlessConfig, HeadlessGame};

/// An adapter for a single app, a peer cluster, or another step-based session.
pub trait BatchSession {
    type Outcome;

    /// Advance one step. An error ends this session, not the rest of the batch.
    fn step(&mut self) -> anyhow::Result<()>;

    /// Poll before the first step and after each successful step.
    fn outcome(&self) -> Option<Self::Outcome>;

    fn exit(&self) -> Option<AppExit> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct BatchConfig {
    pub session_count: u32,
    /// Per-session step cap, also bounded by `headless.frame_budget` if set.
    pub max_frames: u64,
    /// Base config. Session `i` uses `seed.wrapping_add(i)`.
    pub headless: HeadlessConfig,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            session_count: 1,
            max_frames: 600,
            headless: HeadlessConfig::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOutcome<T> {
    Completed(T),
    /// The app exited before reporting an outcome, or exited with an error.
    AppExited {
        code: u8,
    },
    FrameBudgetExhausted,
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResult<T> {
    pub index: u32,
    pub seed: u64,
    /// Number of calls to `step`, including a failing step.
    pub steps: u64,
    pub outcome: SessionOutcome<T>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchReport<T> {
    /// In ascending session index order, with no wall-clock measurements.
    pub sessions: Vec<SessionResult<T>>,
}

/// A deterministic driver. Caller code must also avoid wall time and other
/// nondeterministic inputs if it expects identical outcomes on replay.
#[derive(Clone, Debug)]
pub struct BatchRunner {
    config: BatchConfig,
}

impl BatchRunner {
    pub fn new(config: BatchConfig) -> Self {
        Self { config }
    }

    /// Build a fresh session for each index and run it to completion or its cap.
    pub fn run<S: BatchSession>(
        &self,
        mut build: impl FnMut(u32, &HeadlessConfig) -> anyhow::Result<S>,
    ) -> BatchReport<S::Outcome> {
        let mut config = self.config.headless.clone();
        let limit = config.frame_budget.map_or(self.config.max_frames, |cap| {
            cap.min(self.config.max_frames)
        });
        config.frame_budget = Some(limit);

        let sessions = (0..self.config.session_count)
            .map(|index| {
                config.seed = self.config.headless.seed.wrapping_add(u64::from(index));
                let mut steps = 0;
                let outcome = match build(index, &config) {
                    Ok(mut session) => loop {
                        let exit = session.exit();
                        if let Some(AppExit::Error(code)) = exit {
                            break SessionOutcome::AppExited { code: code.get() };
                        }
                        if let Some(outcome) = session.outcome() {
                            break SessionOutcome::Completed(outcome);
                        }
                        if exit.is_some() {
                            break SessionOutcome::AppExited { code: 0 };
                        }
                        if steps == limit {
                            break SessionOutcome::FrameBudgetExhausted;
                        }
                        steps += 1;
                        if let Err(error) = session.step() {
                            break failure(&error);
                        }
                    },
                    Err(error) => failure(&error),
                };
                SessionResult {
                    index,
                    seed: config.seed,
                    steps,
                    outcome,
                }
            })
            .collect();
        BatchReport { sessions }
    }

    /// For each seed, supply actions in frame order. Apply one per frame and,
    /// once the iterator is exhausted, continue updating without input.
    pub fn run_game<G: HeadlessGame, I: IntoIterator<Item = G::Action>>(
        &self,
        mut actions: impl FnMut(&HeadlessConfig) -> I,
    ) -> BatchReport<G::Outcome> {
        self.run(|_, cfg| {
            Ok(GameSession::<G, _> {
                app: HeadlessApp::for_game::<G>(cfg.clone()),
                actions: actions(cfg).into_iter().fuse(),
                game: PhantomData,
            })
        })
    }
}

fn failure<T>(error: &anyhow::Error) -> SessionOutcome<T> {
    SessionOutcome::Failed {
        message: format!("{error:#}"),
    }
}

struct GameSession<G, I> {
    app: HeadlessApp,
    actions: I,
    game: PhantomData<fn() -> G>,
}

impl<G: HeadlessGame, I: Iterator<Item = G::Action>> BatchSession for GameSession<G, I> {
    type Outcome = G::Outcome;

    fn step(&mut self) -> anyhow::Result<()> {
        if let Some(action) = self.actions.next() {
            self.app.apply_action::<G>(action)?;
        }
        self.app.update();
        Ok(())
    }

    fn outcome(&self) -> Option<Self::Outcome> {
        self.app.outcome::<G>()
    }

    fn exit(&self) -> Option<AppExit> {
        self.app.app_exit()
    }
}
