//! Opt-in panic reporting using a caller-maintained context.

use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{self, Write as _},
    panic::PanicHookInfo,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Debug)]
pub struct PanicHookConfig {
    pub app_name: String,
    pub banner: String,
}

impl Default for PanicHookConfig {
    fn default() -> Self {
        Self {
            app_name: String::from("Application"),
            banner: String::from("Crash report"),
        }
    }
}

#[derive(Clone)]
pub struct CrashContext(Arc<CrashContextInner>);

struct CrashContextInner {
    instance_id: String,
    output_dir: PathBuf,
    snapshot: Mutex<Snapshot>,
    written: AtomicBool,
}

#[derive(Default)]
struct Snapshot {
    tick: u64,
    rng_seed: Option<[u8; 32]>,
    phase: String,
    recent_events: VecDeque<String>,
    state: String,
}

impl CrashContext {
    pub fn new(instance_id: impl Into<String>, output_dir: impl Into<PathBuf>) -> Self {
        Self(Arc::new(CrashContextInner {
            instance_id: instance_id.into(),
            output_dir: output_dir.into(),
            snapshot: Mutex::default(),
            written: AtomicBool::new(false),
        }))
    }

    pub fn set_tick(&self, tick: u64) {
        self.0.snapshot.lock().tick = tick;
    }

    pub fn set_rng_seed(&self, seed: [u8; 32]) {
        self.0.snapshot.lock().rng_seed = Some(seed);
    }

    pub fn set_phase(&self, phase: impl Into<String>) {
        let phase = phase.into();
        self.0.snapshot.lock().phase = phase;
    }

    pub fn record_event(&self, event: impl Into<String>) {
        let event = event.into();
        let mut snapshot = self.0.snapshot.lock();
        if snapshot.recent_events.len() == 64 {
            snapshot.recent_events.pop_front();
        }
        snapshot.recent_events.push_back(event);
    }

    pub fn set_state_snapshot(&self, state: impl Into<String>) {
        let state = state.into();
        self.0.snapshot.lock().state = state;
    }

    fn write_report(&self, config: &PanicHookConfig, info: &PanicHookInfo<'_>) -> io::Result<()> {
        if self.0.written.swap(true, Ordering::Relaxed) {
            return Ok(());
        }

        let now = chrono::Utc::now();
        let mut report = format!(
            "=== {} ===\nApplication: {}\nTimestamp: {}\nInstance: {}\nThread: {:?}\n\n{info}\n",
            config.banner,
            config.app_name,
            now.to_rfc3339(),
            self.0.instance_id,
            std::thread::current().name(),
        );
        if let Some(snapshot) = self.0.snapshot.try_lock() {
            let _ = writeln!(
                report,
                "\nTick: {}\nPhase: {}",
                snapshot.tick, snapshot.phase
            );
            if let Some(seed) = snapshot.rng_seed {
                let _ = write!(report, "RNG seed: ");
                for byte in seed {
                    let _ = write!(report, "{byte:02x}");
                }
                report.push('\n');
            }
            let _ = writeln!(report, "\n--- Recent events ---");
            for event in &snapshot.recent_events {
                let _ = writeln!(report, "{event}");
            }
            let _ = writeln!(report, "\n--- State snapshot ---\n{}", snapshot.state);
        } else {
            report.push_str("\nCrash context unavailable: lock busy.\n");
        }

        fs::create_dir_all(&self.0.output_dir)?;
        let path = self.0.output_dir.join(format!(
            "crash-{}-{}.txt",
            now.format("%Y%m%d-%H%M%S"),
            uuid::Uuid::new_v4(),
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)?.write_all(report.as_bytes())
    }
}

/// Install a process-wide hook, preserving the previous hook. Install once
/// during startup. Each context handles at most one panic and never waits on
/// its snapshot lock. Only put data safe to persist in the context.
pub fn install_panic_hook(config: &PanicHookConfig, context: CrashContext) {
    let config = config.clone();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Err(error) = context.write_report(&config, info) {
            eprintln!("Could not write panic report: {error}");
        }
        previous(info);
    }));
}

#[cfg(feature = "bevy")]
#[derive(bevy::prelude::Resource, Clone)]
pub struct CrashContextRes(pub CrashContext);
