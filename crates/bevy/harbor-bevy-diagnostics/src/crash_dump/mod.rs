//! Local crash bundles containing recent logs and caller-provided snapshots.

mod files;
#[cfg(feature = "minidumper")]
mod minidumper;
#[cfg(feature = "bevy")]
mod plugin;

pub use files::cleanup_old_crash_dumps;
#[cfg(feature = "minidumper")]
pub use minidumper::run_crash_reporter_process_if_requested;
#[cfg(feature = "bevy")]
pub use plugin::{CrashDumpPlugin, CrashDumpResource, release_log_layer};

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;

use crate::log_capture::{LogBuffer, SessionLogLayer, SessionLogSink};
use files::{write_file, write_manifest};

#[derive(Clone, Debug)]
pub struct CrashDumpConfig {
    pub app_name: String,
    pub instance_id: String,
    /// Environment variable overriding the crash-bundle root, if set and nonempty.
    pub root_env_var: Option<String>,
    /// If present in the environment, suppress native reporter startup.
    pub test_mode_env_var: Option<String>,
    /// Flag reserved for the native child reporter, including the leading `--`.
    pub reporter_flag: String,
    /// Dedicated crash-bundle root, before applying any environment override.
    pub base_dir: PathBuf,
    pub log_buffer_entries: usize,
    /// Zero flushes each snapshot immediately.
    pub snapshot_flush_interval_ticks: u64,
}

impl Default for CrashDumpConfig {
    fn default() -> Self {
        Self {
            app_name: "Application".into(),
            instance_id: uuid::Uuid::new_v4().simple().to_string(),
            root_env_var: None,
            test_mode_env_var: None,
            reporter_flag: "--diagnostics-crash-reporter".into(),
            base_dir: "crash-dumps".into(),
            log_buffer_entries: 1024,
            snapshot_flush_interval_ticks: 30,
        }
    }
}

impl CrashDumpConfig {
    #[cfg(feature = "bevy")]
    pub fn for_app(identity: &harbor_bevy_core::AppIdentity) -> Self {
        Self {
            app_name: identity.application.clone(),
            base_dir: identity.data_dir().join("crash-dumps"),
            ..Self::default()
        }
    }

    pub fn root_dir(&self) -> PathBuf {
        self.root_env_var
            .as_ref()
            .and_then(std::env::var_os)
            .filter(|path| !path.is_empty())
            .map_or_else(|| self.base_dir.clone(), PathBuf::from)
    }

    pub fn session_dir(&self) -> Result<PathBuf, CrashDumpError> {
        validate_instance_id(&self.instance_id)?;
        Ok(self.root_dir().join(&self.instance_id))
    }
}

fn validate_instance_id(id: &str) -> Result<(), CrashDumpError> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(CrashDumpError::InvalidInstanceId(id.to_owned()));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CrashDumpError {
    #[error("invalid crash session ID {0:?}; use ASCII letters, numbers, hyphens or underscores")]
    InvalidInstanceId(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[cfg(feature = "minidumper")]
    #[error("invalid native crash reporter argument {0:?}")]
    InvalidReporterArg(String),
    #[cfg(feature = "minidumper")]
    #[error(transparent)]
    Minidumper(#[from] minidumper_child::Error),
}

/// Text and JSON prepared outside the panic handler.
#[derive(Clone, Debug, Default)]
pub struct RuntimeSnapshot {
    pub state_snapshot: String,
    pub metrics_json: String,
    pub event_audit_json: String,
    pub state_hashes_json: String,
}

#[derive(Clone)]
pub struct CrashDumpSession {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    config: CrashDumpConfig,
    session_dir: PathBuf,
    created_at: String,
    log_sink: SessionLogSink,
    snapshot: Mutex<RuntimeSnapshot>,
    terminal_written: AtomicBool,
    #[cfg(feature = "minidumper")]
    native_reporter: Mutex<Option<minidumper_child::ClientHandle>>,
}

impl CrashDumpSession {
    /// Create a fresh bundle. Does not change the process's panic hook or start native capture.
    pub fn new(config: CrashDumpConfig) -> Result<Self, CrashDumpError> {
        let session_dir = config.session_dir()?;
        fs::create_dir_all(
            session_dir
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&session_dir)?;
        let session = Self {
            inner: Arc::new(SessionInner {
                log_sink: SessionLogSink::new(
                    session_dir.join("app.log"),
                    config.log_buffer_entries,
                )?,
                config,
                session_dir,
                created_at: chrono::Utc::now().to_rfc3339(),
                snapshot: Mutex::default(),
                terminal_written: AtomicBool::new(false),
                #[cfg(feature = "minidumper")]
                native_reporter: Mutex::new(None),
            }),
        };
        session.flush_files(&session.inner.snapshot.lock(), Some("[]".into()))?;
        Ok(session)
    }

    pub fn config(&self) -> &CrashDumpConfig {
        &self.inner.config
    }

    pub fn session_dir(&self) -> &Path {
        &self.inner.session_dir
    }

    pub fn log_buffer(&self) -> LogBuffer {
        self.inner.log_sink.buffer()
    }

    pub fn log_layer(&self) -> SessionLogLayer {
        self.inner.log_sink.layer()
    }

    #[cfg(feature = "bevy")]
    pub fn boxed_log_layer(&self) -> bevy::log::BoxedLayer {
        self.inner.log_sink.boxed_layer()
    }

    pub fn update_runtime_snapshot(
        &self,
        snapshot: RuntimeSnapshot,
        tick: u64,
    ) -> Result<(), CrashDumpError> {
        let mut current = self.inner.snapshot.lock();
        *current = snapshot;
        let interval = self.config().snapshot_flush_interval_ticks;
        if interval == 0 || tick.is_multiple_of(interval) {
            self.flush_files(&current, Some(self.log_buffer().to_json()))?;
        }
        Ok(())
    }

    /// Preserve the previous process-wide panic hook and write one panic companion.
    /// Install once during startup, not from a panic handler.
    pub fn install_panic_hook(&self) {
        let previous = std::panic::take_hook();
        let session = self.clone();
        std::panic::set_hook(Box::new(move |info| {
            if let Err(error) = session.write_terminal("panic.txt", || {
                format!(
                    "{} panic companion\nTimestamp: {}\nThread: {:?}\n{info}\n",
                    session.config().app_name,
                    chrono::Utc::now().to_rfc3339(),
                    std::thread::current().name()
                )
            }) {
                eprintln!("Could not write crash bundle: {error}");
            }
            previous(info);
        }));
    }

    #[cfg(feature = "bevy")]
    pub fn record_fatal_code(
        &self,
        error: &dyn harbor_bevy_core::FatalCode,
    ) -> Result<(), CrashDumpError> {
        self.write_terminal("fatal_error.json", || {
            serde_json::json!({
                "written_at": chrono::Utc::now().to_rfc3339(),
                "label": error.label(),
                "exit_code": error.exit_code(),
                "message": error.to_string(),
            })
            .to_string()
        })
    }

    fn write_terminal(
        &self,
        filename: &str,
        body: impl FnOnce() -> String,
    ) -> Result<(), CrashDumpError> {
        if self.inner.terminal_written.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let result = write_file(&self.session_dir().join(filename), body().as_bytes());
        if result.is_err() {
            self.inner.terminal_written.store(false, Ordering::Release);
        }
        result?;
        // Don't wait on a lock that the panicking thread might already hold.
        if let Some(snapshot) = self.inner.snapshot.try_lock() {
            self.flush_files(&snapshot, self.log_buffer().try_to_json())?;
        } else {
            self.refresh_manifest()?;
        }
        Ok(())
    }

    fn flush_files(&self, snapshot: &RuntimeSnapshot, logs: Option<String>) -> io::Result<()> {
        let dir = self.session_dir();
        write_file(
            &dir.join("state_snapshot.txt"),
            snapshot.state_snapshot.as_bytes(),
        )?;
        for (name, content) in [
            ("metrics.json", &snapshot.metrics_json),
            ("event_audit.json", &snapshot.event_audit_json),
            ("state_hashes.json", &snapshot.state_hashes_json),
        ] {
            write_file(
                &dir.join(name),
                if content.is_empty() {
                    b"{}"
                } else {
                    content.as_bytes()
                },
            )?;
        }
        if let Some(logs) = logs {
            write_file(&dir.join("recent_events.json"), logs.as_bytes())?;
        }
        self.refresh_manifest()
    }

    fn refresh_manifest(&self) -> io::Result<()> {
        write_manifest(
            self.session_dir(),
            &self.config().app_name,
            &self.config().instance_id,
            &self.inner.created_at,
        )
    }
}

static GLOBAL_SESSION: Mutex<Option<CrashDumpSession>> = Mutex::new(None);

/// Initialize the global session once. If already initialized, return its handle.
pub fn init_global_crash_dump(config: CrashDumpConfig) -> Result<CrashDumpSession, CrashDumpError> {
    let mut global = GLOBAL_SESSION.lock();
    if let Some(session) = global.as_ref() {
        return Ok(session.clone());
    }
    let session = CrashDumpSession::new(config)?;
    *global = Some(session.clone());
    Ok(session)
}

pub fn global_crash_dump_session() -> Option<CrashDumpSession> {
    GLOBAL_SESSION.lock().clone()
}

#[cfg(feature = "bevy")]
pub fn record_fatal_code(error: &dyn harbor_bevy_core::FatalCode) -> Result<(), CrashDumpError> {
    if let Some(session) = global_crash_dump_session() {
        session.record_fatal_code(error)?;
    }
    Ok(())
}
