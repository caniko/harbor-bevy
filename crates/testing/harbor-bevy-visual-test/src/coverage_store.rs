//! Transactional, queryable SQLite history for fixture and surface coverage.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::{
    FixtureSpec,
    report::{CellOutcome, CellRecord, ReportValidationError, RunReport},
};

pub const COVERAGE_DB_ENV: &str = "HARBOR_VISUAL_COVERAGE_DB";

pub fn db_path(default_path: &Path) -> PathBuf {
    std::env::var_os(COVERAGE_DB_ENV)
        .filter(|path| !path.is_empty())
        .map_or_else(|| default_path.to_owned(), PathBuf::from)
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS visual_runs (
    id TEXT PRIMARY KEY,
    recorded_at_unix_secs INTEGER NOT NULL DEFAULT (unixepoch()),
    git_sha TEXT,
    git_dirty INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('passed', 'failed', 'updated')),
    report_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ui_surfaces (id TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS scenarios (id TEXT PRIMARY KEY, fixture_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS scenario_surfaces (
    scenario_id TEXT NOT NULL REFERENCES scenarios(id),
    surface_id TEXT NOT NULL REFERENCES ui_surfaces(id),
    PRIMARY KEY(scenario_id, surface_id)
);
CREATE TABLE IF NOT EXISTS scenario_anchors (
    scenario_id TEXT NOT NULL REFERENCES scenarios(id),
    anchor TEXT NOT NULL,
    PRIMARY KEY(scenario_id, anchor)
);
CREATE TABLE IF NOT EXISTS viewport_captures (
    id INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES visual_runs(id),
    scenario_id TEXT NOT NULL REFERENCES scenarios(id),
    viewport_label TEXT NOT NULL,
    width INTEGER NOT NULL CHECK(width > 0),
    height INTEGER NOT NULL CHECK(height > 0),
    baseline_path TEXT,
    actual_path TEXT,
    diff_path TEXT,
    UNIQUE(run_id, scenario_id, viewport_label)
);
CREATE TABLE IF NOT EXISTS validation_results (
    capture_id INTEGER NOT NULL REFERENCES viewport_captures(id),
    surface_id TEXT NOT NULL REFERENCES ui_surfaces(id),
    kind TEXT NOT NULL CHECK(kind IN ('audit', 'ssim', 'rubric')),
    status TEXT NOT NULL CHECK(status IN ('passed', 'failed', 'skipped')),
    score REAL,
    threshold REAL,
    PRIMARY KEY(capture_id, surface_id, kind)
);
CREATE INDEX IF NOT EXISTS validation_surface_history ON validation_results(surface_id, kind, capture_id);
";

#[derive(Debug, Error)]
pub enum CoverageError {
    #[error("invalid fixture metadata: {0}")]
    Fixture(String),
    #[error(transparent)]
    Report(#[from] ReportValidationError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}

macro_rules! sql_enum {
    ($ty:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $ty { $($variant),+ }
        impl $ty {
            fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
            fn read(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<Self> {
                match row.get_ref(column)?.as_str()? {
                    $($text => Ok(Self::$variant),)+
                    _ => Err(rusqlite::Error::InvalidQuery),
                }
            }
        }
    };
}

sql_enum!(ValidationKind { Audit => "audit", Ssim => "ssim", Rubric => "rubric" });
sql_enum!(ValidationStatus { Passed => "passed", Failed => "failed", Skipped => "skipped" });

#[derive(Clone, Debug, PartialEq)]
pub struct ValidationRecord {
    pub scenario: String,
    pub viewport_label: String,
    pub surface_id: String,
    pub kind: ValidationKind,
    pub status: ValidationStatus,
    pub score: Option<f64>,
    pub threshold: Option<f64>,
}

pub struct CoverageStore {
    conn: Connection,
}

impl CoverageStore {
    pub fn open(path: &Path) -> Result<Self, CoverageError> {
        fs::create_dir_all(crate::parent_dir(path))?;
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn open_default(default_path: &Path) -> Result<Self, CoverageError> {
        Self::open(&db_path(default_path))
    }

    /// Commit the validated report and its coverage projection together.
    /// Duplicate run IDs are rejected, preserving prior run evidence.
    pub fn record_report(
        &mut self,
        report: &RunReport,
        fixtures: &[FixtureSpec],
    ) -> Result<(), CoverageError> {
        report.validate()?;
        let mut by_id = BTreeMap::new();
        for fixture in fixtures {
            fixture.validate().map_err(CoverageError::Fixture)?;
            if by_id.insert(fixture.name.as_str(), fixture).is_some() {
                return Err(CoverageError::Fixture(format!(
                    "duplicate fixture {:?}",
                    fixture.name
                )));
            }
        }
        let tx = self.conn.transaction()?;
        let status = if !report.failures.is_empty() {
            "failed"
        } else if report.config.update_snapshots {
            "updated"
        } else {
            "passed"
        };
        tx.execute(
            "INSERT INTO visual_runs(id, git_sha, git_dirty, status, report_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![report.run_id, report.git.sha, report.git.dirty, status, serde_json::to_string(report)?],
        )?;
        for fixture in fixtures {
            tx.execute(
                "INSERT INTO scenarios(id, fixture_json) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET fixture_json = excluded.fixture_json",
                params![fixture.name, serde_json::to_string(fixture)?],
            )?;
            tx.execute(
                "DELETE FROM scenario_surfaces WHERE scenario_id = ?1",
                [&fixture.name],
            )?;
            tx.execute(
                "DELETE FROM scenario_anchors WHERE scenario_id = ?1",
                [&fixture.name],
            )?;
            for id in &fixture.surface_ids {
                tx.execute("INSERT OR IGNORE INTO ui_surfaces(id) VALUES (?1)", [id])?;
                tx.execute(
                    "INSERT OR IGNORE INTO scenario_surfaces(scenario_id, surface_id) VALUES (?1, ?2)",
                    params![fixture.name, id],
                )?;
            }
            for anchor in &fixture.anchors {
                tx.execute(
                    "INSERT OR IGNORE INTO scenario_anchors(scenario_id, anchor) VALUES (?1, ?2)",
                    params![fixture.name, anchor],
                )?;
            }
        }
        for cell in &report.cells {
            let Some(fixture) = by_id.get(cell.scenario.as_str()).filter(|fixture| {
                fixture.viewports.iter().any(|viewport| {
                    viewport.label == cell.viewport_label
                        && viewport.width == cell.width
                        && viewport.height == cell.height
                        && viewport.aspect_family == cell.aspect_family
                }) && fixture.ssim_threshold == cell.ssim_threshold
                    && fixture.audit.is_some() == cell.audit_required
                    && fixture.rubric.is_some() == cell.rubric_required
                    && cell.audit.as_ref().is_none_or(|result| {
                        fixture
                            .audit
                            .as_ref()
                            .is_some_and(|spec| spec.checks.len() == result.checked)
                    })
            }) else {
                return Err(CoverageError::Fixture(format!(
                    "metadata does not match {:?} / {:?}",
                    cell.scenario, cell.viewport_label
                )));
            };
            let capture_id = tx.query_row(
                "INSERT INTO viewport_captures(run_id, scenario_id, viewport_label, width, height,
                    baseline_path, actual_path, diff_path) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) RETURNING id",
                params![
                    report.run_id, cell.scenario, cell.viewport_label, cell.width, cell.height,
                    cell.baseline_path.as_ref().map(|path| path.to_string_lossy()),
                    cell.actual_path.as_ref().map(|path| path.to_string_lossy()),
                    cell.diff_path.as_ref().map(|path| path.to_string_lossy()),
                ],
                |row| row.get::<_, i64>(0),
            )?;
            for (kind, status, score, threshold) in validations_for(cell) {
                tx.execute(
                    "INSERT INTO validation_results(capture_id, surface_id, kind, status, score, threshold)
                     SELECT ?1, surface_id, ?2, ?3, ?4, ?5 FROM scenario_surfaces WHERE scenario_id = ?6",
                    params![capture_id, kind.as_str(), status.as_str(), score, threshold, fixture.name],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_report(&self, run_id: &str) -> Result<Option<RunReport>, CoverageError> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT report_json FROM visual_runs WHERE id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        let report = json
            .map(|value| serde_json::from_str::<RunReport>(&value))
            .transpose()?;
        if let Some(report) = &report {
            report.validate()?;
        }
        Ok(report)
    }

    pub fn anchors(&self, scenario: &str) -> rusqlite::Result<Vec<String>> {
        self.conn
            .prepare("SELECT anchor FROM scenario_anchors WHERE scenario_id = ?1 ORDER BY anchor")?
            .query_map([scenario], |row| row.get(0))?
            .collect()
    }

    pub fn validations(&self, run_id: &str) -> rusqlite::Result<Vec<ValidationRecord>> {
        self.conn.prepare(
            "SELECT vc.scenario_id, vc.viewport_label, vr.surface_id, vr.kind, vr.status, vr.score, vr.threshold
             FROM validation_results vr JOIN viewport_captures vc ON vc.id = vr.capture_id
             WHERE vc.run_id = ?1 ORDER BY vc.scenario_id, vc.viewport_label, vr.surface_id, vr.kind",
        )?.query_map([run_id], |row| {
            Ok(ValidationRecord {
                scenario: row.get(0)?, viewport_label: row.get(1)?, surface_id: row.get(2)?,
                kind: ValidationKind::read(row, 3)?, status: ValidationStatus::read(row, 4)?,
                score: row.get(5)?, threshold: row.get(6)?,
            })
        })?.collect()
    }
}

type Validation = (ValidationKind, ValidationStatus, Option<f64>, Option<f64>);

fn validations_for(cell: &CellRecord) -> impl Iterator<Item = Validation> {
    use ValidationKind::{Audit, Rubric, Ssim};
    use ValidationStatus::{Failed, Passed, Skipped};

    let (status, score) = match cell.outcome {
        CellOutcome::Pass { ssim, .. }
        | CellOutcome::RubricFail { ssim, .. }
        | CellOutcome::RubricError { ssim, .. } => (Passed, Some(ssim)),
        CellOutcome::SsimFail { score } => (Failed, Some(score)),
        CellOutcome::BaselineMissing => (Failed, None),
        _ => (Skipped, None),
    };
    let audit = cell.audit.as_ref().map_or(Skipped, |audit| {
        if audit.failures.is_empty() {
            Passed
        } else {
            Failed
        }
    });
    let rubric = match cell.outcome {
        CellOutcome::Pass {
            rubric: Some(_), ..
        } => Passed,
        CellOutcome::RubricFail { .. } | CellOutcome::RubricError { .. } => Failed,
        _ => Skipped,
    };
    [
        Some((Ssim, status, score, Some(cell.ssim_threshold))),
        cell.audit_required.then_some((Audit, audit, None, None)),
        cell.rubric_required.then_some((Rubric, rubric, None, None)),
    ]
    .into_iter()
    .flatten()
}
