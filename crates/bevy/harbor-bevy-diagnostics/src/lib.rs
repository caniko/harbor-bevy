//! Structured logs, local reports, and diagnostics for applications and batch runs.
//!
//! The default build is Bevy-free. Enable `bevy` for ECS integration and crash
//! bundle scheduling, or `minidumper` for optional native crash capture.

pub mod batch_report;
pub mod crash_dump;
pub mod event_audit;
pub mod log_capture;
pub mod metrics;
pub mod panic_hook;
#[cfg(feature = "bevy")]
pub mod plugin;
pub mod remote_spool;
pub mod state_hashing;
pub mod tracing_config;

pub use batch_report::{
    DiagnosticReport, FailureCategory, LogRingBuffer, StateDiffEntry, StateDiffReport,
    ViolationSink,
};
#[cfg(feature = "minidumper")]
pub use crash_dump::run_crash_reporter_process_if_requested;
pub use crash_dump::{
    CrashDumpConfig, CrashDumpError, CrashDumpSession, cleanup_old_crash_dumps,
    global_crash_dump_session, init_global_crash_dump,
};
#[cfg(feature = "bevy")]
pub use crash_dump::{CrashDumpPlugin, CrashDumpResource, record_fatal_code, release_log_layer};
pub use remote_spool::{clear_remote_spool, spool_dir};

#[doc(hidden)]
pub use tracing as __tracing;

/// A structured log entry with an RFC 3339 timestamp.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
}

impl LogEntry {
    pub fn new(level: LogLevel, target: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level,
            target: target.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Trace,
            Self::Debug,
            Self::Info,
            Self::Warn,
            Self::Error,
        ]
        .into_iter()
        .find(|level| value.eq_ignore_ascii_case(level.as_str()))
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Collects logs for later inclusion in a report.
#[derive(Debug, Default)]
pub struct LogCollector(Vec<LogEntry>);

impl LogCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    pub fn push(&mut self, entry: LogEntry) {
        self.0.push(entry);
    }

    pub fn take(self) -> Option<Vec<LogEntry>> {
        (!self.0.is_empty()).then_some(self.0)
    }
}

/// Capture a formatted message, timestamp, and calling module in a collector.
#[macro_export]
macro_rules! capture_log {
    ($collector:expr, $level:ident, $($arg:tt)+) => {
        $collector.push($crate::LogEntry::new(
            $crate::LogLevel::parse(stringify!($level)).expect("valid log level"),
            module_path!(),
            format!($($arg)+),
        ))
    };
}
