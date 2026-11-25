//! Run fixture workers in isolated processes, then score their captures in the parent.

use std::{
    collections::BTreeSet,
    fs, io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use bevy::prelude::World;
use thiserror::Error;

use crate::{
    FixtureSpec, Resolution, VisualSuite,
    audit::{AuditInput, AuditNode},
    capture::{CaptureConfig, FrameSettleHook, capture_with_hook, headless_app},
    diff::{SnapshotError, SnapshotOptions, compare_snapshot},
    report::{
        CellOutcome, CellRecord, GitInfo, RateLimitEvent, ReportValidationError, RunConfig,
        RunReport,
    },
    rubric::{RubricBackend, RubricError, RubricVerdictStatus},
    worker_sidecar::{
        RenderedCellDescriptor, WORKER_SIDECAR_ENV, WorkerOutcome, WorkerResult, WorkerSidecar,
        run_worker, write_sidecar_atomic,
    },
};

#[derive(Clone, Debug)]
pub struct PipelineOptions {
    pub baseline_dir: PathBuf,
    pub actual_dir: PathBuf,
    pub worker_timeout: Duration,
    pub update_snapshots: bool,
}

impl PipelineOptions {
    pub fn new(baseline_dir: impl Into<PathBuf>, actual_dir: impl Into<PathBuf>) -> Self {
        Self {
            baseline_dir: baseline_dir.into(),
            actual_dir: actual_dir.into(),
            worker_timeout: Duration::from_secs(120),
            update_snapshots: cfg!(feature = "update-snapshots"),
        }
    }
}

pub type AuditSource<'a> =
    dyn Fn(&mut World, &FixtureSpec, &Resolution) -> Result<Vec<AuditNode>, String> + 'a;

#[derive(Default)]
pub struct WorkerHooks<'a> {
    pub settle: Option<&'a FrameSettleHook<'a>>,
    /// Caller extracts visible nodes for an audit; required for geometry/text checks.
    pub audit_nodes: Option<&'a AuditSource<'a>>,
}

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("invalid visual run: {0}")]
    Configuration(String),
    #[error(transparent)]
    Report(#[from] ReportValidationError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Entry point for a caller's worker executable. Pass a fixture from
/// `VisualSuite::fixtures()` and the capture directory supplied by the parent.
/// Handled setup errors and panics go in the sidecar, not the exit code.
pub fn run_worker_fixture(
    suite: &impl VisualSuite,
    fixture: &FixtureSpec,
    actual_dir: &Path,
    hooks: &WorkerHooks<'_>,
) -> io::Result<()> {
    let sidecar_path = std::env::var_os(WORKER_SIDECAR_ENV).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{WORKER_SIDECAR_ENV} is missing"),
        )
    })?;
    let sidecar = render_fixture(suite, fixture, actual_dir, hooks);
    write_sidecar_atomic(Path::new(&sidecar_path), &sidecar)
}

fn render_fixture(
    suite: &impl VisualSuite,
    fixture: &FixtureSpec,
    actual_dir: &Path,
    hooks: &WorkerHooks<'_>,
) -> WorkerSidecar {
    let mut sidecar = WorkerSidecar::new(&fixture.name, viewport_labels(fixture));
    if let Err(message) = fixture.validate() {
        sidecar.outcome = WorkerOutcome::SetupError { message };
        return sidecar;
    }
    if fixture
        .audit
        .as_ref()
        .is_some_and(crate::AuditSpec::needs_nodes)
        && hooks.audit_nodes.is_none()
    {
        sidecar.outcome = WorkerOutcome::SetupError {
            message: "geometry/text audit needs an observer".into(),
        };
        return sidecar;
    }
    for viewport in &fixture.viewports {
        match catch_unwind(AssertUnwindSafe(|| {
            render_cell(suite, fixture, viewport, actual_dir, hooks)
        })) {
            Ok(Ok(cell)) => sidecar.cells.push(cell),
            Ok(Err(message)) => {
                sidecar.outcome = WorkerOutcome::SetupError { message };
                break;
            }
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("render worker panicked");
                sidecar.outcome = WorkerOutcome::PartialPanic {
                    message: message.into(),
                    panicked_viewport: Some(viewport.label.clone()),
                };
                break;
            }
        }
    }
    sidecar
}

fn render_cell(
    suite: &impl VisualSuite,
    fixture: &FixtureSpec,
    viewport: &Resolution,
    actual_dir: &Path,
    hooks: &WorkerHooks<'_>,
) -> Result<RenderedCellDescriptor, String> {
    let mut app = headless_app();
    suite.build(fixture, &mut app);
    let actual_path = cell_path(actual_dir, fixture, viewport, "actual.png");
    let config = CaptureConfig::for_fixture(fixture, viewport, &actual_path);
    capture_with_hook(&mut app, &config, hooks.settle).map_err(|error| error.to_string())?;
    let audit = fixture
        .audit
        .as_ref()
        .map(|spec| {
            let nodes = match hooks.audit_nodes {
                Some(source) => source(app.world_mut(), fixture, viewport)?,
                None => vec![],
            };
            let pixels = image::open(&actual_path)
                .map_err(|error| error.to_string())?
                .into_rgba8();
            let input = AuditInput {
                width: viewport.width,
                height: viewport.height,
                nodes,
            };
            spec.evaluate(&input, Some(&pixels))
                .map_err(|error| error.to_string())
        })
        .transpose()?;
    Ok(RenderedCellDescriptor {
        viewport: viewport.clone(),
        actual_path,
        audit,
    })
}

/// Parent entry point. `command` returns the consumer's executable invocation
/// for one fixture and receives the absolute capture directory to pass through.
/// Each planned cell gets an outcome, even after a timeout or protocol error.
pub fn run_pipeline(
    suite: &impl VisualSuite,
    run_id: impl Into<String>,
    git: GitInfo,
    options: &PipelineOptions,
    rubric: Option<&dyn RubricBackend>,
    command: impl Fn(&FixtureSpec, &Path) -> Command,
) -> Result<RunReport, PipelineError> {
    let fixtures = suite.fixtures();
    let run_id = run_id.into();
    if fixtures.is_empty() || options.worker_timeout.is_zero() || run_id.trim().is_empty() {
        return Err(PipelineError::Configuration(
            "fixtures, timeout, and run ID must be nonempty".into(),
        ));
    }
    let mut names = BTreeSet::new();
    for fixture in &fixtures {
        fixture.validate().map_err(PipelineError::Configuration)?;
        if !names.insert(fixture.name.to_ascii_lowercase()) {
            return Err(PipelineError::Configuration(format!(
                "duplicate fixture {:?}",
                fixture.name
            )));
        }
    }
    fs::create_dir_all(&options.actual_dir)?;
    fs::create_dir_all(&options.baseline_dir)?;
    let actual_dir = options.actual_dir.canonicalize()?;
    let baseline_dir = options.baseline_dir.canonicalize()?;
    if actual_dir.starts_with(&baseline_dir) || baseline_dir.starts_with(&actual_dir) {
        return Err(PipelineError::Configuration(
            "baseline and actual directories must be separate".into(),
        ));
    }

    let mut report = RunReport::new(
        run_id,
        git,
        RunConfig {
            update_snapshots: options.update_snapshots,
            rubric_enabled: rubric.is_some(),
            worker_timeout_ms: Some(milliseconds(options.worker_timeout)),
        },
    );
    for fixture in fixtures {
        // ponytail: one renderer worker at a time avoids contention on the pinned GPU.
        // If serial runtime becomes a problem, parallelize scoring before rendering.
        let result = run_worker(command(&fixture, &actual_dir), options.worker_timeout);
        let masks = suite.masks(&fixture);
        let (sidecar, failure) = match result {
            WorkerResult::Success(sidecar)
            | WorkerResult::PartialPanic(sidecar)
            | WorkerResult::SetupError {
                sidecar: Some(sidecar),
                ..
            } => {
                if valid_sidecar(&fixture, &actual_dir, &sidecar) {
                    (Some(sidecar), None)
                } else {
                    (
                        None,
                        Some(CellOutcome::WorkerCrashed {
                            message: "worker sidecar did not match the fixture plan".into(),
                        }),
                    )
                }
            }
            WorkerResult::SetupError {
                message,
                sidecar: None,
            } => (None, Some(CellOutcome::SetupFailed { message })),
            WorkerResult::Timeout { elapsed } => (
                None,
                Some(CellOutcome::WorkerTimeout {
                    elapsed_ms: milliseconds(elapsed),
                }),
            ),
            WorkerResult::Crash { message, .. } => {
                (None, Some(CellOutcome::WorkerCrashed { message }))
            }
        };
        for (index, viewport) in fixture.viewports.iter().enumerate() {
            let mut cell = CellRecord::new(&fixture, viewport);
            cell.baseline_path = Some(cell_path(&baseline_dir, &fixture, viewport, "baseline.png"));
            if let Some(sidecar) = &sidecar {
                if let Some(rendered) = sidecar.cells.get(index) {
                    cell.actual_path = Some(rendered.actual_path.clone());
                    cell.audit.clone_from(&rendered.audit);
                    cell.outcome = score_cell(
                        &mut cell,
                        &fixture,
                        &masks,
                        options.update_snapshots,
                        sidecar.outcome == WorkerOutcome::Success,
                        rubric,
                        &mut report.rate_limit_events,
                    );
                } else {
                    cell.outcome = match &sidecar.outcome {
                        WorkerOutcome::PartialPanic { message, .. }
                            if index == sidecar.cells.len() =>
                        {
                            CellOutcome::RenderPanic {
                                message: message.clone(),
                            }
                        }
                        WorkerOutcome::PartialPanic { .. } => CellOutcome::SkippedAfterPanic,
                        WorkerOutcome::SetupError { message } => CellOutcome::SetupFailed {
                            message: message.clone(),
                        },
                        WorkerOutcome::Success => {
                            unreachable!("validated successful worker returned every cell")
                        }
                    };
                }
            } else if let Some(failure) = &failure {
                cell.outcome.clone_from(failure);
            }
            report.cells.push(cell);
        }
    }
    report.refresh_summary();
    report.validate()?;
    Ok(report)
}

fn valid_sidecar(fixture: &FixtureSpec, actual_dir: &Path, sidecar: &WorkerSidecar) -> bool {
    if sidecar.fixture != fixture.name
        || sidecar.viewports_requested != viewport_labels(fixture)
        || sidecar.cells.len() > fixture.viewports.len()
    {
        return false;
    }
    let missing = fixture.viewports.get(sidecar.cells.len());
    let outcome_matches = match &sidecar.outcome {
        WorkerOutcome::Success => missing.is_none(),
        WorkerOutcome::SetupError { message } => missing.is_some() && !message.trim().is_empty(),
        WorkerOutcome::PartialPanic {
            message,
            panicked_viewport,
        } => {
            !message.trim().is_empty()
                && missing.is_some_and(|viewport| {
                    panicked_viewport
                        .as_ref()
                        .is_none_or(|label| label == &viewport.label)
                })
        }
    };
    outcome_matches
        && sidecar
            .cells
            .iter()
            .zip(&fixture.viewports)
            .all(|(cell, viewport)| {
                cell.viewport == *viewport
                    && cell.actual_path == cell_path(actual_dir, fixture, viewport, "actual.png")
                    && match (&cell.audit, &fixture.audit) {
                        (Some(result), Some(spec)) => {
                            result.checked != 0
                                && result.checked == spec.checks.len()
                                && result.failures.len() <= result.checked
                        }
                        (None, None) => true,
                        _ => false,
                    }
            })
}

fn score_cell(
    cell: &mut CellRecord,
    fixture: &FixtureSpec,
    masks: &[crate::Mask],
    update: bool,
    worker_completed: bool,
    backend: Option<&dyn RubricBackend>,
    rate_limits: &mut Vec<RateLimitEvent>,
) -> CellOutcome {
    let setup_error = |message: String| CellOutcome::SetupFailed { message };
    let (Some(actual), Some(baseline)) = (&cell.actual_path, &cell.baseline_path) else {
        return setup_error("missing screenshot paths".into());
    };
    if cell
        .audit
        .as_ref()
        .is_some_and(|audit| !audit.failures.is_empty())
    {
        return CellOutcome::AuditFail;
    }
    match image::image_dimensions(actual) {
        Ok(dimensions) if dimensions == (cell.width, cell.height) => {}
        Ok(dimensions) => {
            return setup_error(format!(
                "actual dimensions {dimensions:?} do not match the fixture"
            ));
        }
        Err(error) => return setup_error(error.to_string()),
    }
    if update && !worker_completed {
        return setup_error("incomplete fixture cannot update baselines".into());
    }
    let options = if update {
        SnapshotOptions::update()
    } else {
        SnapshotOptions::compare(cell.ssim_threshold)
    };
    match compare_snapshot(baseline, actual, masks, options) {
        Ok(None) => CellOutcome::BaselineUpdated,
        Ok(Some(ssim)) => {
            let (Some(backend), Some(rubric)) = (backend, fixture.rubric.as_ref()) else {
                return CellOutcome::Pass { ssim, rubric: None };
            };
            match backend.evaluate(actual, rubric) {
                Ok(verdict) if verdict.verdict == RubricVerdictStatus::Pass => CellOutcome::Pass {
                    ssim,
                    rubric: Some(verdict),
                },
                Ok(verdict) => CellOutcome::RubricFail { ssim, verdict },
                Err(error) => {
                    if let RubricError::RateLimited {
                        message,
                        retry_after_ms,
                    } = &error
                    {
                        rate_limits.push(RateLimitEvent {
                            scenario: cell.scenario.clone(),
                            viewport_label: cell.viewport_label.clone(),
                            message: message.clone(),
                            retry_after_ms: *retry_after_ms,
                        });
                    }
                    CellOutcome::RubricError {
                        ssim,
                        message: error.to_string(),
                    }
                }
            }
        }
        Err(SnapshotError::BaselineMissing(_)) => CellOutcome::BaselineMissing,
        Err(SnapshotError::BelowThreshold {
            score, diff_path, ..
        }) => {
            cell.diff_path = Some(diff_path);
            CellOutcome::SsimFail { score }
        }
        Err(error) => setup_error(error.to_string()),
    }
}

fn viewport_labels(fixture: &FixtureSpec) -> Vec<String> {
    fixture
        .viewports
        .iter()
        .map(|viewport| viewport.label.clone())
        .collect()
}

fn cell_path(root: &Path, fixture: &FixtureSpec, viewport: &Resolution, file: &str) -> PathBuf {
    root.join(&fixture.name).join(&viewport.label).join(file)
}

fn milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
