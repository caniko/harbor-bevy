//! A versioned, atomic result channel for isolated render workers.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Resolution, audit::AuditReport};

pub const SIDECAR_VERSION: u32 = 1;
pub const WORKER_SIDECAR_ENV: &str = "HARBOR_VISUAL_WORKER_SIDECAR";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedCellDescriptor {
    pub viewport: Resolution,
    pub actual_path: PathBuf,
    pub audit: Option<AuditReport>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerOutcome {
    Success,
    PartialPanic {
        message: String,
        panicked_viewport: Option<String>,
    },
    SetupError {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerSidecar {
    pub version: u32,
    pub fixture: String,
    pub viewports_requested: Vec<String>,
    pub outcome: WorkerOutcome,
    pub cells: Vec<RenderedCellDescriptor>,
}

impl WorkerSidecar {
    pub fn new(fixture: impl Into<String>, viewports_requested: Vec<String>) -> Self {
        Self {
            version: SIDECAR_VERSION,
            fixture: fixture.into(),
            viewports_requested,
            outcome: WorkerOutcome::Success,
            cells: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("unsupported sidecar version {found}, expected {expected}")]
    UnsupportedVersion { found: u32, expected: u32 },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Parse(#[from] serde_json::Error),
}

pub fn write_sidecar_atomic(path: &Path, sidecar: &WorkerSidecar) -> io::Result<()> {
    write_json_atomic(path, sidecar)
}

pub fn read_sidecar(path: &Path) -> Result<WorkerSidecar, SidecarError> {
    #[derive(Deserialize)]
    struct Header {
        version: u32,
    }
    let bytes = fs::read(path)?;
    let header: Header = serde_json::from_slice(&bytes)?;
    if header.version != SIDECAR_VERSION {
        return Err(SidecarError::UnsupportedVersion {
            found: header.version,
            expected: SIDECAR_VERSION,
        });
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn write_json_atomic(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let parent = crate::parent_dir(path);
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(file.as_file_mut(), value)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[derive(Debug)]
pub enum WorkerResult {
    Success(WorkerSidecar),
    PartialPanic(WorkerSidecar),
    SetupError {
        message: String,
        sidecar: Option<WorkerSidecar>,
    },
    Timeout {
        elapsed: Duration,
    },
    Crash {
        status: Option<ExitStatus>,
        message: String,
    },
}

/// Give the child a fresh `HARBOR_VISUAL_WORKER_SIDECAR` path. Workers report
/// handled failures in their sidecar and exit normally. A nonzero exit or a
/// missing/malformed sidecar never counts as a successful run.
///
/// Stderr is inherited for renderer diagnostics; stdin and stdout are detached.
pub fn run_worker(mut command: Command, timeout: Duration) -> WorkerResult {
    let setup_error = |message: String| WorkerResult::SetupError {
        message,
        sidecar: None,
    };
    let dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => return setup_error(error.to_string()),
    };
    let path = dir.path().join("result.json");
    command
        .env(WORKER_SIDECAR_ENV, &path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return setup_error(error.to_string()),
    };
    let start = Instant::now();
    let status = match wait(&mut child, timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            terminate(&mut child);
            return WorkerResult::Timeout {
                elapsed: start.elapsed(),
            };
        }
        Err(error) => {
            terminate(&mut child);
            return WorkerResult::Crash {
                status: None,
                message: error.to_string(),
            };
        }
    };
    if !status.success() {
        return WorkerResult::Crash {
            status: Some(status),
            message: status.to_string(),
        };
    }
    match read_sidecar(&path) {
        Ok(sidecar) => match &sidecar.outcome {
            WorkerOutcome::Success => WorkerResult::Success(sidecar),
            WorkerOutcome::PartialPanic { .. } => WorkerResult::PartialPanic(sidecar),
            WorkerOutcome::SetupError { message } => WorkerResult::SetupError {
                message: message.clone(),
                sidecar: Some(sidecar),
            },
        },
        Err(error) => WorkerResult::Crash {
            status: Some(status),
            message: error.to_string(),
        },
    }
}

#[cfg(any(unix, windows))]
fn wait(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    wait_timeout::ChildExt::wait_timeout(child, timeout)
}

#[cfg(not(any(unix, windows)))]
fn wait(_child: &mut Child, _timeout: Duration) -> io::Result<Option<ExitStatus>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "isolated worker timeouts require Unix or Windows",
    ))
}

fn terminate(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id()) {
        use nix::{
            sys::signal::{Signal, killpg},
            unistd::Pid,
        };
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}
