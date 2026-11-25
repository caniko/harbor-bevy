use std::sync::Arc;

use bevy::{log::BoxedLayer, prelude::*};
use harbor_bevy_core::GracefulShutdownRequest;

use super::{CrashDumpSession, RuntimeSnapshot, global_crash_dump_session};
use crate::{
    event_audit::{EventAuditState, audit_events_end_frame},
    metrics::SimulationMetrics,
    state_hashing::StateHashLog,
};

#[derive(Resource, Clone)]
pub struct CrashDumpResource(pub CrashDumpSession);

#[derive(Resource, Clone)]
struct SnapshotProvider(Arc<dyn Fn(&World) -> String + Send + Sync>);

/// Schedule caller-defined snapshots. Can be used with or without
/// [`crate::plugin::DiagnosticsPlugin`]; installed metrics are included if available.
pub struct CrashDumpPlugin {
    session: CrashDumpSession,
    provider: SnapshotProvider,
}

impl CrashDumpPlugin {
    pub fn new(
        session: CrashDumpSession,
        provider: impl Fn(&World) -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            session,
            provider: SnapshotProvider(Arc::new(provider)),
        }
    }
}

impl Plugin for CrashDumpPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(CrashDumpResource(self.session.clone()))
            .insert_resource(self.session.log_buffer())
            .insert_resource(self.provider.clone())
            .add_systems(Last, persist_snapshot.after(audit_events_end_frame))
            .add_observer(record_fatal);
    }
}

/// Supply to `bevy::log::LogPlugin::custom_layer` when using a global session.
pub fn release_log_layer(app: &mut App) -> Option<BoxedLayer> {
    let session = global_crash_dump_session()?;
    app.insert_resource(session.log_buffer());
    Some(session.boxed_log_layer())
}

fn persist_snapshot(world: &World, mut tick: Local<u64>) {
    *tick = tick.saturating_add(1);
    let session = &world.resource::<CrashDumpResource>().0;
    let provider = &world.resource::<SnapshotProvider>().0;
    let snapshot = RuntimeSnapshot {
        state_snapshot: provider(world),
        metrics_json: world
            .get_resource::<SimulationMetrics>()
            .map_or_else(String::new, SimulationMetrics::to_json),
        event_audit_json: world
            .get_resource::<EventAuditState>()
            .map_or_else(String::new, EventAuditState::to_json),
        state_hashes_json: world
            .get_resource::<StateHashLog>()
            .map_or_else(String::new, StateHashLog::to_json),
    };
    if let Err(error) = session.update_runtime_snapshot(snapshot, *tick) {
        tracing::warn!(%error, "could not persist crash bundle snapshot");
    }
}

fn record_fatal(request: On<GracefulShutdownRequest>, session: Res<CrashDumpResource>) {
    if let Err(error) = session.0.record_fatal_code(request.error.as_ref()) {
        tracing::warn!(%error, "could not record fatal code");
    }
}
