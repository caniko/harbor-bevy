//! Stateful property tests and deterministic replay for headless Bevy apps.
//!
//! Implement [`FuzzModel`] and call [`run_fuzz`] to generate a fresh action
//! strategy from the live world at each step. A saved [`CorpusEntry`] records
//! concrete actions, so replay never calls the generator. Determinism also
//! requires the model to avoid uncontrolled inputs such as wall time.
//! Let the harness drive updates: `HeadlessGame::apply` should dispatch the
//! action without updating the app itself.

use std::collections::BTreeMap;

use harbor_bevy_headless::{HeadlessApp, HeadlessGame};
use proptest::strategy::BoxedStrategy;

pub mod corpus;
pub mod replay;
pub mod runner;

pub use corpus::{CorpusEntry, CorpusStore, TraceConfig};
pub use replay::{replay, replay_file, replay_hash};
pub use runner::{FuzzConfig, FuzzFailure, FuzzReport, FuzzRunResult, run_fuzz, run_proptest};

/// A headless game with a state-dependent action generator and safety checks.
pub trait FuzzModel: HeadlessGame {
    /// Build an owned strategy from the current app. Called after startup, and
    /// again after the previous action and its updates.
    fn strategy(app: &HeadlessApp) -> BoxedStrategy<Self::Action>;

    /// Check after startup, every successful action, and each subsequent frame.
    fn invariants(app: &HeadlessApp) -> Result<(), Violation>;
}

/// A model-defined failure with a stable rule identifier and optional details.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{rule} at frame {frame}: {message}")]
pub struct Violation {
    pub message: String,
    pub rule: String,
    /// Number of completed updates; use [`HeadlessApp::frames`].
    pub frame: u64,
    pub context: BTreeMap<String, serde_json::Value>,
}

impl Violation {
    pub fn new(rule: impl Into<String>, message: impl Into<String>, frame: u64) -> Self {
        Self {
            message: message.into(),
            rule: rule.into(),
            frame,
            context: BTreeMap::new(),
        }
    }
}
