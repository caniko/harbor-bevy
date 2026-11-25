//! Structured reports and bounded log snapshots for batch runs.

use crate::{LogEntry, LogLevel};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;

#[derive(Debug)]
pub struct LogRingBuffer {
    entries: VecDeque<LogEntry>,
    capacity: usize,
}

impl LogRingBuffer {
    /// Zero capacity disables retention.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity,
        }
    }

    pub fn push(&mut self, entry: LogEntry) {
        if self.capacity > 0 {
            if self.entries.len() == self.capacity {
                self.entries.pop_front();
            }
            self.entries.push_back(entry);
        }
    }

    pub fn log(&mut self, level: &str, target: &str, message: String) {
        self.push(LogEntry::new(
            LogLevel::parse(level).unwrap_or(LogLevel::Info),
            target,
            message,
        ));
    }

    pub fn iter(&self) -> impl Iterator<Item = &LogEntry> {
        self.entries.iter()
    }

    pub fn snapshot(self) -> Vec<LogEntry> {
        self.entries.into()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum FailureCategory {
    ConnectionTimeout,
    Timeout,
    ValidationError,
    Desync,
    Panic,
    WatchdogKill,
    #[default]
    Other,
}

impl FailureCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConnectionTimeout => "ConnectionTimeout",
            Self::Timeout => "Timeout",
            Self::ValidationError => "ValidationError",
            Self::Desync => "Desync",
            Self::Panic => "Panic",
            Self::WatchdogKill => "WatchdogKill",
            Self::Other => "Other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StateDiffEntry {
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_value: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StateDiffReport {
    pub entries: Vec<StateDiffEntry>,
    pub left_hash: Option<String>,
    pub right_hash: Option<String>,
}

impl StateDiffReport {
    /// Compare caller-defined top-level fields. Nested values are compared as a whole.
    pub fn from_snapshots(left: &Map<String, Value>, right: &Map<String, Value>) -> Self {
        let fields: BTreeSet<_> = left.keys().chain(right.keys()).collect();
        let entries = fields
            .into_iter()
            .filter_map(|field| {
                let (l, r) = (left.get(field), right.get(field));
                (l != r).then(|| StateDiffEntry {
                    field: field.clone(),
                    left_value: l.cloned(),
                    right_value: r.cloned(),
                })
            })
            .collect();
        Self {
            entries,
            ..Self::default()
        }
    }

    pub fn has_diffs(&self) -> bool {
        !self.entries.is_empty()
            || self
                .left_hash
                .as_ref()
                .zip(self.right_hash.as_ref())
                .is_some_and(|(left, right)| left != right)
    }

    pub fn to_text(&self) -> String {
        let mut out = String::from("=== State Diff ===\n");
        if let (Some(left), Some(right)) = (&self.left_hash, &self.right_hash) {
            let _ = writeln!(out, "Hashes: left={left}, right={right}");
        }
        for entry in &self.entries {
            let render = |value: &Option<Value>| {
                value
                    .as_ref()
                    .map_or_else(|| "<missing>".into(), Value::to_string)
            };
            let _ = writeln!(
                out,
                "{}: left={}, right={}",
                entry.field,
                render(&entry.left_value),
                render(&entry.right_value)
            );
        }
        out
    }

    pub fn to_json(&self) -> String {
        serde_json::json!(self).to_string()
    }
}

/// Diagnostics collected for a failed run. Snapshot and metadata schemas belong to the caller.
#[derive(Clone, Debug, Default, Serialize)]
pub struct DiagnosticReport {
    pub session_id: u32,
    pub failure_category: FailureCategory,
    pub elapsed_secs: f64,
    pub description: String,
    pub left_state: Option<Value>,
    pub right_state: Option<Value>,
    pub state_diff: Option<StateDiffReport>,
    pub logs: Vec<LogEntry>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub extra: Value,
}

impl DiagnosticReport {
    pub fn to_json(&self) -> String {
        serde_json::json!(self).to_string()
    }
}

/// Thread-safe collection of reports shared by concurrent workers.
#[derive(Clone, Default)]
pub struct ViolationSink(Arc<Mutex<Vec<DiagnosticReport>>>);

impl ViolationSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, report: DiagnosticReport) {
        self.0.lock().push(report);
    }

    pub fn len(&self) -> usize {
        self.0.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drain all collected reports, including when other handles still exist.
    pub fn into_reports(self) -> Vec<DiagnosticReport> {
        std::mem::take(&mut *self.0.lock())
    }

    pub fn summary(&self) -> String {
        let reports = self.0.lock();
        let mut out = format!("=== Diagnostic Reports ({}) ===\n", reports.len());
        for report in &*reports {
            let _ = writeln!(
                out,
                "Session {} ({}): {}",
                report.session_id,
                report.failure_category.as_str(),
                report.description
            );
            for error in &report.errors {
                let _ = writeln!(out, "  ERROR: {error}");
            }
            for warning in &report.warnings {
                let _ = writeln!(out, "  WARN: {warning}");
            }
        }
        out
    }
}
