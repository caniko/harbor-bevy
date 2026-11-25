//! Deterministic JSON reports. The caller supplies the run ID and Git metadata.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    AspectFamily, FixtureSpec, Resolution,
    audit::AuditReport,
    rubric::{RubricVerdict, RubricVerdictStatus},
    worker_sidecar::write_json_atomic,
};

pub const RUN_REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitInfo {
    pub sha: Option<String>,
    pub branch: Option<String>,
    pub dirty: bool,
}

impl GitInfo {
    pub fn from_repo(path: &Path) -> io::Result<Self> {
        let git = |args: &[&str]| -> io::Result<String> {
            let output = Command::new("git").current_dir(path).args(args).output()?;
            if !output.status.success() {
                return Err(io::Error::other(
                    String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                ));
            }
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        };
        let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        Ok(Self {
            sha: Some(git(&["rev-parse", "HEAD"])?),
            branch: (branch != "HEAD").then_some(branch),
            dirty: !git(&["status", "--porcelain"])?.is_empty(),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunConfig {
    pub update_snapshots: bool,
    pub rubric_enabled: bool,
    pub worker_timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CellOutcome {
    Pass {
        ssim: f64,
        rubric: Option<RubricVerdict>,
    },
    BaselineUpdated,
    BaselineMissing,
    SsimFail {
        score: f64,
    },
    RubricFail {
        ssim: f64,
        verdict: RubricVerdict,
    },
    RubricError {
        ssim: f64,
        message: String,
    },
    AuditFail,
    SetupFailed {
        message: String,
    },
    RenderPanic {
        message: String,
    },
    SkippedAfterPanic,
    WorkerTimeout {
        elapsed_ms: u64,
    },
    WorkerCrashed {
        message: String,
    },
}

impl CellOutcome {
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass { .. } | Self::BaselineUpdated)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellRecord {
    pub scenario: String,
    pub viewport_label: String,
    pub aspect_family: Option<AspectFamily>,
    pub width: u32,
    pub height: u32,
    pub baseline_path: Option<PathBuf>,
    pub actual_path: Option<PathBuf>,
    pub diff_path: Option<PathBuf>,
    pub ssim_threshold: f64,
    pub audit_required: bool,
    pub audit: Option<AuditReport>,
    pub rubric_required: bool,
    pub outcome: CellOutcome,
}

impl CellRecord {
    /// Start with a nonpassing outcome so an unexecuted cell cannot pass.
    pub fn new(fixture: &FixtureSpec, viewport: &Resolution) -> Self {
        Self {
            scenario: fixture.name.clone(),
            viewport_label: viewport.label.clone(),
            aspect_family: viewport.aspect_family.clone(),
            width: viewport.width,
            height: viewport.height,
            baseline_path: None,
            actual_path: None,
            diff_path: None,
            ssim_threshold: fixture.ssim_threshold,
            audit_required: fixture.audit.is_some(),
            audit: None,
            rubric_required: fixture.rubric.is_some(),
            outcome: CellOutcome::SetupFailed {
                message: "cell did not run".into(),
            },
        }
    }

    fn key(&self) -> (&str, &str) {
        (&self.scenario, &self.viewport_label)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub total_cells: usize,
    pub passed_cells: usize,
    pub failed_cells: usize,
    pub baselines_updated: usize,
    pub ssim_pass: usize,
    pub ssim_fail: usize,
    pub ssim_skipped: usize,
    pub rubric_pass: usize,
    pub rubric_fail: usize,
    pub rubric_skipped: usize,
    pub audit_pass: usize,
    pub audit_fail: usize,
    pub baseline_missing: usize,
    pub setup_failed: usize,
    pub render_panic: usize,
    pub skipped_after_panic: usize,
    pub worker_timeout: usize,
    pub worker_crashed: usize,
    pub scenarios_with_failures: Vec<String>,
}

impl Summary {
    pub fn from_cells(cells: &[CellRecord]) -> Self {
        let mut summary = Self {
            total_cells: cells.len(),
            ..Self::default()
        };
        let mut failures = BTreeSet::new();
        for cell in cells {
            let mut scored = true;
            let mut rubric_ran = false;
            if cell.outcome.is_pass() {
                summary.passed_cells += 1;
            } else {
                summary.failed_cells += 1;
                failures.insert(&cell.scenario);
            }
            if let Some(audit) = &cell.audit {
                if audit.failures.is_empty() {
                    summary.audit_pass += 1;
                } else {
                    summary.audit_fail += 1;
                }
            }
            match &cell.outcome {
                CellOutcome::Pass { rubric, .. } => {
                    summary.ssim_pass += 1;
                    if rubric.is_some() {
                        summary.rubric_pass += 1;
                        rubric_ran = true;
                    }
                }
                CellOutcome::SsimFail { .. } => summary.ssim_fail += 1,
                CellOutcome::RubricFail { .. } | CellOutcome::RubricError { .. } => {
                    summary.ssim_pass += 1;
                    summary.rubric_fail += 1;
                    rubric_ran = true;
                }
                outcome => {
                    scored = false;
                    match outcome {
                        CellOutcome::BaselineUpdated => summary.baselines_updated += 1,
                        CellOutcome::BaselineMissing => summary.baseline_missing += 1,
                        CellOutcome::SetupFailed { .. } => summary.setup_failed += 1,
                        CellOutcome::RenderPanic { .. } => summary.render_panic += 1,
                        CellOutcome::SkippedAfterPanic => summary.skipped_after_panic += 1,
                        CellOutcome::WorkerTimeout { .. } => summary.worker_timeout += 1,
                        CellOutcome::WorkerCrashed { .. } => summary.worker_crashed += 1,
                        _ => {}
                    }
                }
            }
            summary.ssim_skipped += usize::from(!scored);
            summary.rubric_skipped += usize::from(!rubric_ran && cell.rubric_required);
        }
        summary.scenarios_with_failures = failures.into_iter().cloned().collect();
        summary
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RateLimitEvent {
    pub scenario: String,
    pub viewport_label: String,
    pub message: String,
    pub retry_after_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub schema_version: u32,
    pub run_id: String,
    pub git: GitInfo,
    pub config: RunConfig,
    pub summary: Summary,
    pub cells: Vec<CellRecord>,
    pub failures: Vec<CellRecord>,
    pub rate_limit_events: Vec<RateLimitEvent>,
}

#[derive(Debug, Error)]
#[error("invalid visual run report: {}", .0.join("; "))]
pub struct ReportValidationError(pub Vec<String>);

#[derive(Debug, Error)]
pub enum ReportReadError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Parse(#[from] serde_json::Error),
    #[error(transparent)]
    Invalid(#[from] ReportValidationError),
}

impl RunReport {
    pub fn new(run_id: impl Into<String>, git: GitInfo, config: RunConfig) -> Self {
        Self {
            schema_version: RUN_REPORT_SCHEMA_VERSION,
            run_id: run_id.into(),
            git,
            config,
            summary: Summary::default(),
            cells: vec![],
            failures: vec![],
            rate_limit_events: vec![],
        }
    }

    pub fn refresh_summary(&mut self) {
        self.cells.sort_by(|a, b| a.key().cmp(&b.key()));
        self.rate_limit_events.sort();
        self.summary = Summary::from_cells(&self.cells);
        self.failures = self
            .cells
            .iter()
            .filter(|cell| !cell.outcome.is_pass())
            .cloned()
            .collect();
    }

    pub fn write_atomic(&self, path: &Path) -> io::Result<()> {
        self.validate().map_err(io::Error::other)?;
        write_json_atomic(path, self)
    }

    pub fn read(path: &Path) -> Result<Self, ReportReadError> {
        let report: Self = serde_json::from_reader(fs::File::open(path)?)?;
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<(), ReportValidationError> {
        let mut failures = Vec::new();
        if self.schema_version != RUN_REPORT_SCHEMA_VERSION {
            failures.push(format!(
                "unsupported schema version {}",
                self.schema_version
            ));
        }
        if self.run_id.trim().is_empty() || self.cells.is_empty() {
            failures.push("run ID and cells must be nonempty".into());
        }
        if self.summary != Summary::from_cells(&self.cells) {
            failures.push("summary does not match cell outcomes".into());
        }
        if !self
            .failures
            .iter()
            .eq(self.cells.iter().filter(|cell| !cell.outcome.is_pass()))
        {
            failures.push("failure records do not match cell outcomes".into());
        }
        if self
            .cells
            .windows(2)
            .any(|pair| pair[0].key() >= pair[1].key())
        {
            failures.push("cells must be sorted with unique scenario/viewport keys".into());
        }
        if !self.rate_limit_events.is_sorted()
            || self.rate_limit_events.iter().any(|event| {
                event.message.trim().is_empty()
                    || self
                        .cells
                        .binary_search_by(|cell| {
                            cell.key()
                                .cmp(&(event.scenario.as_str(), event.viewport_label.as_str()))
                        })
                        .is_err()
            })
        {
            failures.push("rate limit events must be sorted and refer to planned cells".into());
        }
        for cell in &self.cells {
            if !self.valid_cell(cell) {
                failures.push(format!(
                    "invalid result for {:?} / {:?}",
                    cell.scenario, cell.viewport_label
                ));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(ReportValidationError(failures))
        }
    }

    fn valid_cell(&self, cell: &CellRecord) -> bool {
        if cell.scenario.trim().is_empty()
            || cell.viewport_label.trim().is_empty()
            || cell.width == 0
            || cell.height == 0
            || !(0.0..=1.0).contains(&cell.ssim_threshold)
            || cell.audit.as_ref().is_some_and(|audit| {
                !cell.audit_required || audit.checked == 0 || audit.checked < audit.failures.len()
            })
        {
            return false;
        }
        let captured = cell.actual_path.is_some() && cell.baseline_path.is_some();
        let audit_passed = cell
            .audit
            .as_ref()
            .map_or(!cell.audit_required, |audit| audit.failures.is_empty());
        let rubric_required = self.config.rubric_enabled && cell.rubric_required;
        let scored = |ssim: f64| {
            captured
                && audit_passed
                && !self.config.update_snapshots
                && (cell.ssim_threshold..=1.0).contains(&ssim)
        };
        if cell.diff_path.is_some() != matches!(cell.outcome, CellOutcome::SsimFail { .. })
            || (!audit_passed
                && cell.audit.is_some()
                && !matches!(cell.outcome, CellOutcome::AuditFail))
        {
            return false;
        }
        match &cell.outcome {
            CellOutcome::Pass { ssim, rubric } => {
                scored(*ssim)
                    && if rubric_required {
                        rubric
                            .as_ref()
                            .is_some_and(|result| result.verdict == RubricVerdictStatus::Pass)
                    } else {
                        rubric.is_none()
                    }
            }
            CellOutcome::BaselineUpdated => {
                captured && audit_passed && self.config.update_snapshots
            }
            CellOutcome::BaselineMissing => {
                captured && audit_passed && !self.config.update_snapshots
            }
            CellOutcome::SsimFail { score } => {
                captured
                    && audit_passed
                    && !self.config.update_snapshots
                    && (-1.0..cell.ssim_threshold).contains(score)
            }
            CellOutcome::RubricFail { ssim, verdict } => {
                scored(*ssim) && rubric_required && verdict.verdict == RubricVerdictStatus::Fail
            }
            CellOutcome::RubricError { ssim, message } => {
                scored(*ssim) && rubric_required && !message.trim().is_empty()
            }
            CellOutcome::AuditFail => captured && !audit_passed && cell.audit.is_some(),
            CellOutcome::SetupFailed { message }
            | CellOutcome::RenderPanic { message }
            | CellOutcome::WorkerCrashed { message } => !message.trim().is_empty(),
            CellOutcome::SkippedAfterPanic | CellOutcome::WorkerTimeout { .. } => true,
        }
    }

    pub fn validate_for_pass(&self) -> Result<(), ReportValidationError> {
        self.validate()?;
        if self.failures.is_empty() && !self.config.update_snapshots {
            Ok(())
        } else {
            Err(ReportValidationError(vec![
                "visual verification requires a non-update run with no failures".into(),
            ]))
        }
    }
}
