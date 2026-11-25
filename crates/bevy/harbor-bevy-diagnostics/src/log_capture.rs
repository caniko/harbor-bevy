//! Tracing capture into a bounded buffer and an append-only local log.

use crate::{LogEntry, LogLevel, LogRingBuffer};
use parking_lot::Mutex;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_subscriber::{Layer, layer::Context};

#[derive(Clone)]
#[cfg_attr(feature = "bevy", derive(bevy::prelude::Resource))]
pub struct LogBuffer(Arc<Mutex<LogRingBuffer>>);

impl LogBuffer {
    pub fn new(max_entries: usize) -> Self {
        Self(Arc::new(Mutex::new(LogRingBuffer::new(max_entries))))
    }

    pub fn push(&self, entry: LogEntry) {
        self.0.lock().push(entry);
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.0.lock().iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.0.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn to_json(&self) -> String {
        serde_json::json!(self.entries()).to_string()
    }

    /// Panics may run while a lock is held. Do not wait for that lock.
    pub(crate) fn try_to_json(&self) -> Option<String> {
        let entries: Vec<_> = self.0.try_lock()?.iter().cloned().collect();
        Some(serde_json::json!(entries).to_string())
    }
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new(500)
    }
}

#[derive(Clone)]
pub struct SessionLogSink {
    buffer: LogBuffer,
    file: Arc<Mutex<File>>,
    path: PathBuf,
}

impl SessionLogSink {
    pub fn new(path: impl Into<PathBuf>, max_entries: usize) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        Ok(Self {
            buffer: LogBuffer::new(max_entries),
            file: Arc::new(Mutex::new(file)),
            path,
        })
    }

    pub fn buffer(&self) -> LogBuffer {
        self.buffer.clone()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn layer(&self) -> SessionLogLayer {
        SessionLogLayer(self.clone())
    }

    #[cfg(feature = "bevy")]
    pub fn boxed_layer(&self) -> bevy::log::BoxedLayer {
        Box::new(self.layer())
    }

    pub fn write_line(&self, line: &str) -> io::Result<()> {
        self.file.lock().write_all(format!("{line}\n").as_bytes())
    }

    pub fn append_entry(&self, entry: LogEntry) -> io::Result<()> {
        let line = format!(
            "{} [{}] {}: {}\n",
            escape_line(&entry.timestamp),
            entry.level,
            escape_line(&entry.target),
            escape_line(&entry.message)
        );
        let mut file = self.file.lock();
        self.buffer.push(entry);
        file.write_all(line.as_bytes())
    }
}

fn escape_line(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

#[derive(Default)]
struct MessageVisitor(Option<String>);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = Some(value.to_string());
        }
    }
}

pub struct SessionLogLayer(SessionLogSink);

impl<S: Subscriber> Layer<S> for SessionLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut message = MessageVisitor::default();
        event.record(&mut message);
        let entry = LogEntry::new(
            LogLevel::parse(meta.level().as_str()).unwrap_or(LogLevel::Info),
            meta.target(),
            message.0.unwrap_or_else(|| meta.name().to_string()),
        );
        // Tracing cannot return sink errors to the caller. The ring still has the event.
        let _ = self.0.append_entry(entry);
    }
}
