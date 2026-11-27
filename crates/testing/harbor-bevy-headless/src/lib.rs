//! Deterministic, manually stepped Bevy apps and test reports.
//!
//! Drive real Bevy schedules without rendering, network I/O, or wall-clock
//! waits imposed by the harness. Games still own their system ordering and
//! any external inputs.
//!
//! ```
//! use bevy::prelude::*;
//! use harbor_bevy_headless::HeadlessApp;
//!
//! #[derive(Resource, Default)]
//! struct Ticks(u32);
//!
//! let mut app = HeadlessApp::default();
//! app.app_mut()
//!     .init_resource::<Ticks>()
//!     .add_systems(FixedUpdate, |mut ticks: ResMut<Ticks>| ticks.0 += 1);
//! assert_eq!(app.advance_fixed(3), 3);
//! assert_eq!(app.resource::<Ticks>().unwrap().0, 3);
//! ```
//!
//! Use [`HeadlessGame`] to reuse the same game adapter in [`BatchRunner`], or
//! [`PeerCluster`] to explicitly interleave multiple peers.
//!
//! Enable `schedule-shuffle` to run a game under different Bevy system
//! orderings with `ScheduleShuffleConfig::run_game`. Supply a stable final
//! state hash; add `report.axis_report()` to a [`MatrixReport`] to gate CI.

pub mod app;
pub mod batch;
pub mod bug_report;
pub mod cadence_audit;
pub mod fault_injector;
pub mod matrix_report;
pub mod peer_cluster;
#[cfg(feature = "schedule-shuffle")]
mod schedule_shuffle;

pub use app::{HeadlessApp, HeadlessConfig, HeadlessGame};
pub use batch::{
    BatchConfig, BatchReport, BatchRunner, BatchSession, SessionOutcome, SessionResult,
};
pub use bug_report::{FieldDifference, SessionBugReport, SnapshotField, StateSnapshot};
pub use cadence_audit::CadenceAudit;
pub use fault_injector::{
    FaultAction, FaultInjectionEnvelope, FaultInjector, FaultInjectorPlugin, FaultSync, OpKey,
    probe_fault,
};
pub use harbor_bevy_diagnostics::{FailureCategory, LogEntry, LogLevel, LogRingBuffer};
pub use matrix_report::{AxisReport, MatrixReport, ScenarioOutcome, ScenarioResult};
pub use peer_cluster::{PeerCluster, PeerIndex};
#[cfg(feature = "schedule-shuffle")]
pub use schedule_shuffle::{
    ScheduleOrderSample, ScheduleOrderVariance, ScheduleShuffleCase, ScheduleShuffleConfig,
    ScheduleShuffleReport, TraceShuffleCase, TraceShuffleConfig, TraceShuffleReport,
    capture_schedule_order, parse_schedule_seeds,
};

fn first_differing_tick<T: PartialEq>(baseline: &[T], actual: &[T]) -> Option<usize> {
    baseline
        .iter()
        .zip(actual)
        .position(|(a, b)| a != b)
        .or_else(|| (baseline.len() != actual.len()).then(|| baseline.len().min(actual.len())))
        .map(|i| i + 1)
}
