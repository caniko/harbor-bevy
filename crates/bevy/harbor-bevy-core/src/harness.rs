use std::{
    fs,
    io::{self, Write},
    num::NonZeroU8,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bevy::{app::AppExit, log::warn, prelude::*, time::Real};
use serde::{Deserialize, Serialize};

use crate::write_then_rename;

/// Application-supplied status for the next heartbeat.
#[derive(Debug, Default, Clone, Resource, Serialize)]
pub struct ProgressSample {
    pub phase: String,
    pub round: u32,
    pub turn: u32,
    pub elapsed_phase_secs: f32,
}

/// Controls one heartbeat stream. Give each writer its own output path.
#[derive(Debug, Resource)]
pub struct ProgressHeartbeatConfig {
    path: PathBuf,
    pub interval_secs: f32,
    pub trace_writes: bool,
    pub run_id: Option<String>,
    elapsed: f32,
    progress_seq: u64,
    last_progress: Option<(String, u32, u32)>,
}

impl ProgressHeartbeatConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            interval_secs: 2.0,
            trace_writes: false,
            run_id: None,
            elapsed: 0.0,
            progress_seq: 0,
            last_progress: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn observe_progress(&mut self, sample: &ProgressSample) {
        let changed = self
            .last_progress
            .as_ref()
            .is_none_or(|(phase, round, turn)| {
                (phase, *round, *turn) != (&sample.phase, sample.round, sample.turn)
            });
        if changed {
            if self.last_progress.is_some() {
                self.progress_seq = self.progress_seq.saturating_add(1);
            }
            self.last_progress = Some((sample.phase.clone(), sample.round, sample.turn));
        }
    }
}

impl Drop for ProgressHeartbeatConfig {
    fn drop(&mut self) {
        if self.path.file_name().is_some() {
            let _ = fs::remove_file(&self.path);
            let _ = fs::remove_file(temporary_path(&self.path));
        }
    }
}

#[derive(Serialize)]
struct Heartbeat<'a> {
    pid: u32,
    timestamp_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<&'a str>,
    progress_seq: u64,
    #[serde(flatten)]
    sample: &'a ProgressSample,
}

/// Periodically publish a JSON heartbeat using real, not simulation, time.
///
/// Install `TimePlugin` and insert a config and sample before running this system.
/// Set `interval_secs` to zero to publish every update.
pub fn write_progress_heartbeat(
    time: Res<Time<Real>>,
    mut config: ResMut<ProgressHeartbeatConfig>,
    sample: Res<ProgressSample>,
) {
    config.elapsed += time.delta_secs();
    if config.elapsed < config.interval_secs {
        return;
    }
    config.elapsed = 0.0;
    config.observe_progress(&sample);

    let heartbeat = Heartbeat {
        pid: std::process::id(),
        timestamp_ms: now_ms(),
        run_id: config.run_id.as_deref(),
        progress_seq: config.progress_seq,
        sample: &sample,
    };
    let result = (|| -> io::Result<()> {
        if config.path.file_name().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a heartbeat filename",
            ));
        }
        if let Some(parent) = config
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(&heartbeat)?;
        write_then_rename(&temporary_path(&config.path), &config.path, &bytes)
    })();
    if let Err(error) = result {
        warn!(
            "Heartbeat write failed ({}): {error}",
            config.path.display()
        );
    } else if config.trace_writes {
        warn!(
            "Heartbeat written ({}): phase={} round={} turn={} progress_seq={}",
            config.path.display(),
            sample.phase,
            sample.round,
            sample.turn,
            config.progress_seq,
        );
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    temporary.into()
}

fn now_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

const STALE_GRACE: Duration = Duration::from_secs(90);

#[derive(Deserialize)]
struct HeartbeatStamp {
    pid: u32,
    timestamp_ms: u64,
}

/// Remove stale or malformed `game_progress_*.json` files and their temp files.
///
/// A heartbeat is stale after 90 seconds or when its process no longer exists
/// on Linux. Old orphan `.json.tmp` files are also discarded. Returns the
/// number of published heartbeats removed. Use a directory shared only by
/// local writers.
pub fn cleanup_stale_heartbeat_files(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let stale_before = now_ms().saturating_sub(STALE_GRACE.as_millis() as u64);
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name
            .to_str()
            .filter(|name| name.starts_with("game_progress_"))
        else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        if name.ends_with(".json.tmp") {
            if entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age >= STALE_GRACE))
            {
                let _ = fs::remove_file(path);
            }
            continue;
        }
        if !name.ends_with(".json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let stale = serde_json::from_slice::<HeartbeatStamp>(&bytes).map_or(true, |stamp| {
            stamp.timestamp_ms <= stale_before || !process_is_alive(stamp.pid)
        });
        if stale && fs::remove_file(&path).is_ok() {
            removed += 1;
            let _ = fs::remove_file(temporary_path(&path));
        }
    }
    removed
}

#[cfg(target_os = "linux")]
fn process_is_alive(pid: u32) -> bool {
    let proc = Path::new("/proc");
    !proc.join("self").exists() || proc.join(pid.to_string()).exists()
}

#[cfg(not(target_os = "linux"))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

pub fn format_ready_marker(detail: &str) -> String {
    if detail.is_empty() {
        "[READY]".to_owned()
    } else {
        format!("[READY] {detail}")
    }
}

/// Emit and flush the marker so an external harness can detect it immediately.
pub fn emit_ready_marker(detail: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", format_ready_marker(detail))?;
    stdout.flush()
}

pub const EXIT_CODE_SUCCESS: u8 = 0;
pub const EXIT_CODE_ERROR: u8 = 1;

pub fn format_exit_code(code: u8) -> String {
    let label = match code {
        EXIT_CODE_SUCCESS => "SUCCESS",
        EXIT_CODE_ERROR => "ERROR",
        _ => "UNKNOWN",
    };
    format!("{label} ({code})")
}

pub fn exit_code_to_app_exit(code: u8) -> AppExit {
    NonZeroU8::new(code).map_or(AppExit::Success, AppExit::Error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_changes_only_when_the_progress_tuple_changes() {
        let path = std::env::temp_dir().join(format!("heartbeat-{}.json", uuid::Uuid::new_v4()));
        let mut config = ProgressHeartbeatConfig::new(path);
        let mut sample = ProgressSample::default();
        config.observe_progress(&sample);
        assert_eq!(config.progress_seq, 0);
        sample.elapsed_phase_secs = 2.0;
        config.observe_progress(&sample);
        assert_eq!(config.progress_seq, 0);
        sample.phase = "next".into();
        config.observe_progress(&sample);
        assert_eq!(config.progress_seq, 1);
        sample.round += 1;
        config.observe_progress(&sample);
        assert_eq!(config.progress_seq, 2);
        sample.turn += 1;
        config.observe_progress(&sample);
        assert_eq!(config.progress_seq, 3);
    }
}
