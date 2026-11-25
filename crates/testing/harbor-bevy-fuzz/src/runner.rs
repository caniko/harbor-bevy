//! Shared trace execution and a seeded proptest campaign.

use std::{
    cell::{Cell, RefCell},
    fmt::Debug,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
};

use bevy::app::AppExit;
use harbor_bevy_headless::{
    FaultInjectionEnvelope, FaultInjector, FaultInjectorPlugin, HeadlessApp, HeadlessConfig,
};
use proptest::{
    prelude::*,
    strategy::ValueTree,
    test_runner::{Config, RngAlgorithm, RngSeed, TestError, TestRunner},
};
use serde::Serialize;
use thiserror::Error;

use crate::{CorpusEntry, CorpusStore, FuzzModel, TraceConfig, Violation};

/// Why a trace failed. Frames count completed updates.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum FuzzFailure {
    #[error("panic at frame {frame}: {message}")]
    Panic { frame: u64, message: String },
    #[error(transparent)]
    InvariantViolation(#[from] Violation),
    #[error("apply or fault setup failed at frame {frame}: {message}")]
    ApplyError { frame: u64, message: String },
    #[error("fatal app exit {code} at frame {frame}")]
    FatalExit { frame: u64, code: u8 },
    #[error("frame budget {budget} exhausted at frame {frame} with trace work remaining")]
    NonTermination { frame: u64, budget: u64 },
    #[error("cannot load replay: {message}")]
    ReplayLoad { message: String },
}

/// A trace reached a terminal outcome, exited successfully, or consumed all its
/// actions and requested frames without violating an invariant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuzzRunResult<T> {
    pub frames: u64,
    pub actions_applied: usize,
    pub outcome: Option<T>,
    pub exited: bool,
}

#[derive(Clone, Debug)]
pub struct FuzzConfig {
    pub cases: u32,
    /// Maximum generated actions per case. Must be positive.
    pub max_steps: usize,
    /// Updates after each action. Zero permits multiple actions between updates.
    pub frames_per_action: u64,
    /// Seeds both the proptest campaign and each app, overriding `headless.seed`.
    pub seed: u64,
    pub corpus_dir: PathBuf,
    pub save_failures: bool,
    pub headless: HeadlessConfig,
    pub fault_envelope: Option<FaultInjectionEnvelope>,
}

impl Default for FuzzConfig {
    fn default() -> Self {
        Self {
            cases: 64,
            max_steps: 16,
            frames_per_action: 1,
            seed: 0,
            corpus_dir: PathBuf::from("fuzz-corpus"),
            save_failures: true,
            headless: HeadlessConfig::default(),
            fault_envelope: None,
        }
    }
}

/// Campaign summary. Stops at the first failure, then shrinks that case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuzzReport {
    pub seed: u64,
    /// Passing cases before shrinking; rejects and shrink attempts don't count.
    pub cases_passed: u32,
    pub failure: Option<FuzzFailure>,
    /// When saving is enabled and a failure occurs, this holds the saved path
    /// or an explanation of why no reproducer was saved.
    pub reproducer: Option<Result<PathBuf, String>>,
}

/// Execute a recorded trace without generating any new actions.
pub fn run_trace<G: FuzzModel>(
    entry: CorpusEntry<G::Action>,
) -> Result<FuzzRunResult<G::Outcome>, FuzzFailure> {
    if entry.config.fixed_dt.is_zero() {
        return Err(FuzzFailure::ReplayLoad {
            message: "fixed_dt must be nonzero".to_owned(),
        });
    }
    let mut actions = entry.actions.into_iter();
    drive::<G>(entry.seed, &entry.config, entry.fault_envelope, |_| {
        actions.next()
    })
}

/// Generate one action at a time and persist the shrunk failing trace, if any.
/// Invalid configuration or too many rejected generations aborts the campaign
/// with an error; game failures and corpus write errors stay in the report.
pub fn run_fuzz<G: FuzzModel>(config: &FuzzConfig) -> anyhow::Result<FuzzReport>
where
    G::Action: Clone + Debug + Serialize,
{
    anyhow::ensure!(
        (1..usize::MAX).contains(&config.max_steps),
        "invalid max_steps"
    );
    anyhow::ensure!(
        !config.headless.fixed_dt.is_zero(),
        "fixed_dt must be nonzero"
    );
    let trace_config = TraceConfig {
        fixed_dt: config.headless.fixed_dt,
        frame_budget: config.headless.frame_budget,
        enable_logging: config.headless.enable_logging,
        frames_per_action: config.frames_per_action,
    };
    let property_config = Config {
        cases: config.cases,
        failure_persistence: None,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(config.seed),
        max_shrink_time: 0,
        ..Config::default()
    };
    let mut runner = TestRunner::new(property_config.clone());
    let cases_passed = Cell::new(0);
    let last_failure = RefCell::new(None);

    // ponytail: shrink by dropping steps and changing their RNG seeds, not by
    // shrinking individual action values. A model-specific shrinker can add
    // value shrinking if this stops producing small enough reproducers.
    let choices = proptest::collection::vec(any::<u64>(), 1..=config.max_steps);
    let result = runner.run(&choices, |choices| {
        let mut choices = choices.into_iter();
        let mut actions = Vec::new();
        let mut rejected = None;
        let mut generating = false;
        let result = drive::<G>(
            config.seed,
            &trace_config,
            config.fault_envelope.clone(),
            |app| {
                let seed = choices.next()?;
                generating = true;
                let mut sampler = TestRunner::new(Config {
                    rng_seed: RngSeed::Fixed(seed),
                    ..property_config.clone()
                });
                let action = match G::strategy(app).new_tree(&mut sampler) {
                    Ok(tree) => Some(tree.current()),
                    Err(reason) => {
                        rejected = Some(reason);
                        None
                    }
                };
                actions.extend(action.iter().cloned());
                generating = false;
                action
            },
        );
        if let Some(reason) = rejected {
            return Err(TestCaseError::reject(reason));
        }
        match result {
            Ok(_) => {
                if last_failure.borrow().is_none() {
                    cases_passed.set(cases_passed.get() + 1);
                }
                Ok(())
            }
            Err(failure) => {
                let reason = failure.to_string();
                *last_failure.borrow_mut() = Some((failure, (!generating).then_some(actions)));
                Err(TestCaseError::fail(reason))
            }
        }
    });

    let mut report = FuzzReport {
        seed: config.seed,
        cases_passed: cases_passed.get(),
        failure: None,
        reproducer: None,
    };
    match result {
        Ok(()) => {}
        Err(TestError::Abort(reason)) => anyhow::bail!("proptest aborted: {reason}"),
        Err(TestError::Fail(reason, _)) => {
            // Use the last observed failing case, not an unexecuted shrink
            // candidate. No extra run or assumptions about replay are needed here.
            let (failure, actions) = last_failure
                .into_inner()
                .ok_or_else(|| anyhow::anyhow!("proptest failed without a trace: {reason}"))?;
            if config.save_failures {
                report.reproducer = Some(match actions {
                    Some(actions) => CorpusStore::new(&config.corpus_dir)
                        .save(&CorpusEntry {
                            name: std::any::type_name::<G>().to_owned(),
                            seed: config.seed,
                            config: trace_config,
                            fault_envelope: config.fault_envelope.clone(),
                            actions,
                        })
                        .map_err(|error| format!("{error:#}")),
                    None => {
                        Err("action generation panicked; replay does not run the strategy".into())
                    }
                });
            }
            report.failure = Some(failure);
        }
    }
    Ok(report)
}

pub use run_fuzz as run_proptest;

fn drive<G: FuzzModel>(
    seed: u64,
    config: &TraceConfig,
    fault: Option<FaultInjectionEnvelope>,
    mut next: impl FnMut(&HeadlessApp) -> Option<G::Action>,
) -> Result<FuzzRunResult<G::Outcome>, FuzzFailure> {
    let mut frame = 0;
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut app = HeadlessApp::for_game::<G>(HeadlessConfig {
            seed,
            fixed_dt: config.fixed_dt,
            frame_budget: config.frame_budget,
            enable_logging: config.enable_logging,
        });
        if let Some(fault) = fault {
            if !app.app().is_plugin_added::<FaultInjectorPlugin>() {
                app.app_mut().add_plugins(FaultInjectorPlugin);
            }
            let mut injector = app.world_mut().resource_mut::<FaultInjector>();
            injector
                .schedule(fault)
                .map_err(|error| apply_error(frame, &error))?;
        }
        if exited(&app)? {
            return Ok(FuzzRunResult {
                frames: 0,
                actions_applied: 0,
                outcome: None,
                exited: true,
            });
        }
        check_budget(frame, config.frame_budget)?;
        app.update();
        frame = app.frames();
        let mut actions_applied = 0;
        if let Some(result) = inspect::<G>(&app, actions_applied)? {
            return Ok(result);
        }
        while let Some(action) = next(&app) {
            check_budget(frame, config.frame_budget)?;
            app.apply_action::<G>(action)
                .map_err(|error| apply_error(frame, &error))?;
            actions_applied += 1;
            if let Some(result) = inspect::<G>(&app, actions_applied)? {
                return Ok(result);
            }
            for _ in 0..config.frames_per_action {
                check_budget(frame, config.frame_budget)?;
                app.update();
                frame = app.frames();
                if let Some(result) = inspect::<G>(&app, actions_applied)? {
                    return Ok(result);
                }
            }
        }
        Ok(FuzzRunResult {
            frames: frame,
            actions_applied,
            outcome: None,
            exited: false,
        })
    }));
    result.unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("non-string panic payload")
            .to_owned();
        Err(FuzzFailure::Panic { frame, message })
    })
}

fn apply_error(frame: u64, error: &anyhow::Error) -> FuzzFailure {
    FuzzFailure::ApplyError {
        frame,
        message: format!("{error:#}"),
    }
}

fn check_budget(frame: u64, budget: Option<u64>) -> Result<(), FuzzFailure> {
    if let Some(budget) = budget
        && frame >= budget
    {
        return Err(FuzzFailure::NonTermination { frame, budget });
    }
    Ok(())
}

fn inspect<G: FuzzModel>(
    app: &HeadlessApp,
    actions_applied: usize,
) -> Result<Option<FuzzRunResult<G::Outcome>>, FuzzFailure> {
    G::invariants(app)?;
    let exited = exited(app)?;
    let outcome = app.outcome::<G>();
    Ok((exited || outcome.is_some()).then_some(FuzzRunResult {
        frames: app.frames(),
        actions_applied,
        outcome,
        exited,
    }))
}

fn exited(app: &HeadlessApp) -> Result<bool, FuzzFailure> {
    match app.app_exit() {
        Some(AppExit::Error(code)) => Err(FuzzFailure::FatalExit {
            frame: app.frames(),
            code: code.get(),
        }),
        Some(AppExit::Success) => Ok(true),
        None => Ok(false),
    }
}
