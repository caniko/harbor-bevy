//! Structured failure reports and atomic publication to `bug_report.json`.

use std::{
    collections::BTreeMap,
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use harbor_bevy_diagnostics::{FailureCategory, LogEntry, StateDiffReport};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Caller-defined snapshot fields. Omit intentionally peer-local state from
/// snapshots used for comparison, or normalize it before passing it in.
pub type StateSnapshot = Map<String, Value>;

/// Makes an absent field distinct from a field explicitly set to JSON `null`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotField {
    Missing,
    Value(Value),
}

impl From<Option<Value>> for SnapshotField {
    fn from(value: Option<Value>) -> Self {
        value.map_or(Self::Missing, Self::Value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDifference {
    pub peer_0: SnapshotField,
    pub peer_1: SnapshotField,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionBugReport {
    pub session_id: u32,
    /// Initialized from an existing diagnostics category; callers can also
    /// supply their own label without changing the report schema.
    pub failure_category: String,
    pub elapsed_secs: f64,
    pub description: String,
    pub logs: Vec<LogEntry>,
    peer_0: Option<StateSnapshot>,
    peer_1: Option<StateSnapshot>,
    state_diff: Option<BTreeMap<String, FieldDifference>>,
}

impl SessionBugReport {
    pub fn new(
        session_id: u32,
        failure_category: FailureCategory,
        elapsed_secs: f64,
        description: impl Into<String>,
    ) -> Self {
        Self {
            session_id,
            failure_category: failure_category.as_str().into(),
            elapsed_secs,
            description: description.into(),
            logs: Vec::new(),
            peer_0: None,
            peer_1: None,
            state_diff: None,
        }
    }

    /// Store both snapshots and compare top-level fields if both are present.
    /// Nested values are compared as a whole.
    pub fn set_snapshots(&mut self, peer_0: Option<StateSnapshot>, peer_1: Option<StateSnapshot>) {
        self.state_diff = peer_0.as_ref().zip(peer_1.as_ref()).map(|(left, right)| {
            StateDiffReport::from_snapshots(left, right)
                .entries
                .into_iter()
                .map(|entry| {
                    (
                        entry.field,
                        FieldDifference {
                            peer_0: entry.left_value.into(),
                            peer_1: entry.right_value.into(),
                        },
                    )
                })
                .collect()
        });
        self.peer_0 = peer_0;
        self.peer_1 = peer_1;
    }

    pub fn snapshots(&self) -> (Option<&StateSnapshot>, Option<&StateSnapshot>) {
        (self.peer_0.as_ref(), self.peer_1.as_ref())
    }

    pub fn state_diff(&self) -> Option<&BTreeMap<String, FieldDifference>> {
        self.state_diff.as_ref()
    }

    /// Write `bug_report.json` to `dir` and return the published path.
    pub fn write_to_dir(&self, dir: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
        let path = dir.as_ref().join("bug_report.json");
        self.write_to_file(&path)?;
        Ok(path)
    }

    /// Write through a unique sibling temp file and atomically replace the
    /// destination only after serialization and flushing succeed.
    pub fn write_to_file(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.elapsed_secs.is_finite() && self.elapsed_secs >= 0.0,
            "invalid elapsed_secs"
        );
        let path = path.as_ref();
        anyhow::ensure!(path.file_name().is_some(), "expected a report filename");
        let dir = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        {
            let mut writer = BufWriter::new(tmp.as_file_mut());
            serde_json::to_writer_pretty(&mut writer, self)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}
