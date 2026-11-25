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

pub mod app;
pub mod batch;
pub mod bug_report;
pub mod fault_injector;
pub mod matrix_report;
pub mod peer_cluster;

pub use app::{HeadlessApp, HeadlessConfig, HeadlessGame};
pub use batch::{
    BatchConfig, BatchReport, BatchRunner, BatchSession, SessionOutcome, SessionResult,
};
pub use bug_report::{FieldDifference, SessionBugReport, SnapshotField, StateSnapshot};
pub use fault_injector::{
    FaultAction, FaultInjectionEnvelope, FaultInjector, FaultInjectorPlugin, FaultSync, OpKey,
    probe_fault,
};
pub use harbor_bevy_diagnostics::{FailureCategory, LogEntry, LogLevel, LogRingBuffer};
pub use matrix_report::{AxisReport, MatrixReport, ScenarioOutcome, ScenarioResult};
pub use peer_cluster::{PeerCluster, PeerIndex};
