//! Stock Bevy glue for frame metrics, state hashing, event auditing, and panic context.

use bevy::{
    prelude::*,
    time::{Real, Time},
};
use std::path::PathBuf;

use crate::{
    event_audit::{EventAuditConfig, EventAuditState, audit_events_end_frame},
    metrics::{SimulationMetrics, record_frame_time},
    panic_hook::{CrashContext, CrashContextRes, PanicHookConfig, install_panic_hook},
    state_hashing::{StateHashConfig, StateHashLog, periodic_state_hash},
};

#[derive(Clone, Debug)]
pub struct DiagnosticsPluginConfig {
    pub instance_id: String,
    pub output_dir: PathBuf,
    pub hash_interval: u64,
    pub event_audit: EventAuditConfig,
    /// Install a process-wide panic hook only when explicitly configured.
    pub panic_hook: Option<PanicHookConfig>,
}

impl Default for DiagnosticsPluginConfig {
    fn default() -> Self {
        Self {
            instance_id: String::new(),
            output_dir: "logs/diagnostics".into(),
            hash_interval: StateHashConfig::default().interval_ticks,
            event_audit: EventAuditConfig::default(),
            panic_hook: None,
        }
    }
}

#[derive(Default)]
pub struct DiagnosticsPlugin {
    config: DiagnosticsPluginConfig,
}

impl DiagnosticsPlugin {
    pub fn new(config: DiagnosticsPluginConfig) -> Self {
        Self { config }
    }
}

impl Plugin for DiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        let context = CrashContext::new(&self.config.instance_id, &self.config.output_dir);
        if let Some(config) = &self.config.panic_hook {
            install_panic_hook(config, context.clone());
        }
        app.insert_resource(CrashContextRes(context))
            .insert_resource(SimulationMetrics::new(&self.config.instance_id))
            .insert_resource(StateHashConfig {
                interval_ticks: self.config.hash_interval,
                instance_id: self.config.instance_id.clone(),
                ..Default::default()
            })
            .insert_resource(self.config.event_audit.clone())
            .init_resource::<StateHashLog>()
            .init_resource::<EventAuditState>()
            .add_systems(First, update_crash_tick)
            .add_systems(
                PreUpdate,
                record_frame_time.run_if(resource_exists::<Time<Real>>),
            )
            .add_systems(PostUpdate, periodic_state_hash)
            .add_systems(Last, audit_events_end_frame);
    }
}

fn update_crash_tick(context: Res<CrashContextRes>, mut tick: Local<u64>) {
    *tick = tick.saturating_add(1);
    context.0.set_tick(*tick);
}
