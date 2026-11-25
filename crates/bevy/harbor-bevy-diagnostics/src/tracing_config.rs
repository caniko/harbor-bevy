//! Configurable process tracing and thread-local batch log routing.

use parking_lot::Mutex;
use std::{
    cell::Cell,
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TracingTargetDirective {
    pub target: String,
    pub level: Level,
}

impl TracingTargetDirective {
    pub fn new(target: impl Into<String>, level: Level) -> Self {
        Self {
            target: target.into(),
            level,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TracingProfile {
    pub name: String,
    pub aliases: Vec<String>,
    pub directives: Vec<TracingTargetDirective>,
}

#[derive(Clone, Debug)]
pub struct TracingConfig {
    pub default_level: Level,
    pub base_directives: Vec<TracingTargetDirective>,
    pub engine_error_directives: Vec<TracingTargetDirective>,
    pub profile_env_var: Option<String>,
    pub profiles: Vec<TracingProfile>,
    /// `None` sends normal tracing to stderr. Batch tracing requires a directory.
    pub log_dir: Option<PathBuf>,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            default_level: Level::WARN,
            base_directives: Vec::new(),
            engine_error_directives: Vec::new(),
            profile_env_var: None,
            profiles: Vec::new(),
            log_dir: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TracingInitError {
    #[error("batch tracing needs a log directory")]
    MissingLogDir,
    #[error("invalid tracing target {0:?}")]
    InvalidTarget(String),
    #[error(transparent)]
    Filter(#[from] tracing_subscriber::filter::ParseError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Subscriber(#[from] tracing_subscriber::util::TryInitError),
}

impl TracingConfig {
    /// Build the configured filter without reading the process environment.
    /// Profile directives take precedence over matching base/engine directives.
    pub fn filter_for_profile(&self, profile: Option<&str>) -> Result<EnvFilter, TracingInitError> {
        let profile = profile.and_then(|name| {
            self.profiles.iter().find(|profile| {
                profile.name.eq_ignore_ascii_case(name)
                    || profile
                        .aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(name))
            })
        });
        let mut filter = EnvFilter::default().add_directive(self.default_level.into());
        for directive in self
            .base_directives
            .iter()
            .chain(&self.engine_error_directives)
            .chain(profile.into_iter().flat_map(|profile| &profile.directives))
        {
            if directive.target.is_empty()
                || directive
                    .target
                    .chars()
                    .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')))
            {
                return Err(TracingInitError::InvalidTarget(directive.target.clone()));
            }
            filter =
                filter.add_directive(format!("{}={}", directive.target, directive.level).parse()?);
        }
        Ok(filter)
    }

    fn filter_from_env(&self) -> Result<EnvFilter, TracingInitError> {
        if let Ok(filter) = EnvFilter::try_from_default_env() {
            return Ok(filter);
        }
        let profile = self
            .profile_env_var
            .as_ref()
            .and_then(|name| std::env::var(name).ok());
        self.filter_for_profile(profile.as_deref().map(str::trim))
    }
}

/// Install a global subscriber. Keep the returned guard alive to flush file logs.
pub fn init_tracing(config: &TracingConfig) -> Result<Option<WorkerGuard>, TracingInitError> {
    let filter = config.filter_from_env()?;
    let fmt = fmt::layer()
        .compact()
        .with_ansi(false)
        .with_thread_ids(true);
    let subscriber = tracing_subscriber::registry().with(filter);
    if let Some(dir) = &config.log_dir {
        let (writer, guard) = tracing_appender::non_blocking(open_log(&dir.join("tracing.log"))?);
        subscriber.with(fmt.with_writer(writer)).try_init()?;
        Ok(Some(guard))
    } else {
        subscriber.with(fmt.with_writer(io::stderr)).try_init()?;
        Ok(None)
    }
}

thread_local! {
    static ACTIVE_SESSION: Cell<Option<u32>> = const { Cell::new(None) };
    static ACTIVE_PLAYER: Cell<Option<u8>> = const { Cell::new(None) };
}

/// Set the session for the calling thread and clear its previous player.
pub fn set_active_session(id: Option<u32>) {
    ACTIVE_SESSION.set(id);
    ACTIVE_PLAYER.set(None);
}

/// When a session is active, route output to that player's log. `None` uses
/// the session-level log. All player IDs, including zero, are equivalent here.
pub fn set_active_player(id: Option<u8>) {
    ACTIVE_PLAYER.set(id);
}

type Writers = Mutex<HashMap<(u32, Option<u8>), Arc<Mutex<File>>>>;

/// Releases batch file handles on drop. The global subscriber remains installed.
pub struct BatchTracingGuard {
    writers: Arc<Writers>,
}

impl BatchTracingGuard {
    /// Flush and release the cached file handles for a completed session.
    pub fn flush_session(&self, session_id: u32) {
        self.writers.lock().retain(|&(session, _), writer| {
            if session == session_id {
                let _ = writer.lock().flush();
                false
            } else {
                true
            }
        });
    }
}

impl Drop for BatchTracingGuard {
    fn drop(&mut self) {
        for (_, writer) in self.writers.lock().drain() {
            let _ = writer.lock().flush();
        }
    }
}

struct PerSessionMakeWriter {
    writers: Arc<Writers>,
    log_dir: PathBuf,
    fallback: Arc<Mutex<File>>,
}

struct SessionWriter(Arc<Mutex<File>>);

impl Write for SessionWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().write(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.0.lock().write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().flush()
    }
}

impl fmt::MakeWriter<'_> for PerSessionMakeWriter {
    type Writer = SessionWriter;

    fn make_writer(&self) -> Self::Writer {
        let writer = ACTIVE_SESSION.get().and_then(|session| {
            let player = ACTIVE_PLAYER.get();
            let mut writers = self.writers.lock();
            let entry = writers.entry((session, player));
            match entry {
                std::collections::hash_map::Entry::Occupied(entry) => Some(entry.get().clone()),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let mut dir = self.log_dir.join(format!("session-{session:04}"));
                    if let Some(player) = player {
                        dir.push(format!("player-{player}"));
                    }
                    open_log(&dir.join("tracing.log"))
                        .ok()
                        .map(|file| entry.insert(Arc::new(Mutex::new(file))).clone())
                }
            }
        });
        SessionWriter(writer.unwrap_or_else(|| self.fallback.clone()))
    }
}

fn open_log(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Install a global subscriber that routes events to session/player files.
/// Events without a session go to `batch.log`.
pub fn init_batch_tracing(config: &TracingConfig) -> Result<BatchTracingGuard, TracingInitError> {
    let filter = config.filter_from_env()?;
    let dir = config
        .log_dir
        .as_ref()
        .ok_or(TracingInitError::MissingLogDir)?;
    let make_writer = PerSessionMakeWriter {
        writers: Arc::default(),
        log_dir: dir.clone(),
        fallback: Arc::new(Mutex::new(open_log(&dir.join("batch.log"))?)),
    };
    let writers = make_writer.writers.clone();
    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .compact()
                .with_ansi(false)
                .with_thread_ids(true)
                .with_writer(make_writer),
        )
        .try_init()?;
    Ok(BatchTracingGuard { writers })
}

/// Create a span tagged with a simulation instance and tick.
#[macro_export]
macro_rules! sim_span {
    ($instance_id:expr, $tick:expr) => {
        $crate::__tracing::info_span!("sim", instance = %$instance_id, tick = $tick)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_alias_overrides_matching_directive() {
        let config = TracingConfig {
            base_directives: vec![TracingTargetDirective::new("demo", Level::INFO)],
            engine_error_directives: vec![TracingTargetDirective::new("engine", Level::ERROR)],
            profiles: vec![TracingProfile {
                name: "verbose".into(),
                aliases: vec!["detail".into()],
                directives: vec![TracingTargetDirective::new("demo", Level::TRACE)],
            }],
            ..Default::default()
        };
        let base = config.filter_for_profile(None).unwrap().to_string();
        let profile = config
            .filter_for_profile(Some("DETAIL"))
            .unwrap()
            .to_string();
        assert!(base.contains("demo=info"));
        assert!(profile.contains("demo=trace"));
        assert!(!profile.contains("demo=info"));
        assert!(profile.contains("engine=error"));
    }

    #[test]
    fn batch_router_selects_files_and_reopens_in_append_mode() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().to_path_buf();
        let writer = PerSessionMakeWriter {
            writers: Arc::default(),
            fallback: Arc::new(Mutex::new(open_log(&log_dir.join("batch.log")).unwrap())),
            log_dir: log_dir.clone(),
        };
        let guard = BatchTracingGuard {
            writers: writer.writers.clone(),
        };
        let subscriber =
            tracing_subscriber::registry().with(fmt::layer().with_ansi(false).with_writer(writer));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("fallback marker");
            set_active_session(Some(12));
            set_active_player(Some(0));
            tracing::info!("first player marker");
            guard.flush_session(12);
            tracing::info!("second player marker");
            set_active_session(Some(12));
            tracing::info!("session marker");
            set_active_session(None);
        });
        let player = fs::read_to_string(log_dir.join("session-0012/player-0/tracing.log")).unwrap();
        assert!(player.contains("first player marker"));
        assert!(player.contains("second player marker"));
        let session = fs::read_to_string(log_dir.join("session-0012/tracing.log")).unwrap();
        assert!(session.contains("session marker"));
        assert!(!session.contains("player marker"));
        assert!(
            fs::read_to_string(log_dir.join("batch.log"))
                .unwrap()
                .contains("fallback marker")
        );
    }
}
