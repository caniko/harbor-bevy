//! Replay concrete traces using the same executor as generated cases.

use std::path::Path;

use serde::de::DeserializeOwned;

use crate::{CorpusStore, FuzzFailure, FuzzModel, FuzzRunResult};

pub use crate::runner::run_trace as replay;

/// Load and replay a JSON file, regardless of its filename.
pub fn replay_file<G: FuzzModel>(
    path: impl AsRef<Path>,
) -> Result<FuzzRunResult<G::Outcome>, FuzzFailure>
where
    G::Action: DeserializeOwned,
{
    replay::<G>(CorpusStore::load_file(path).map_err(load_error)?)
}

/// Load, verify, and replay an entry by its 16-character corpus ID.
pub fn replay_hash<G: FuzzModel>(
    corpus: &CorpusStore,
    hash: &str,
) -> Result<FuzzRunResult<G::Outcome>, FuzzFailure>
where
    G::Action: DeserializeOwned,
{
    replay::<G>(corpus.load(hash).map_err(load_error)?)
}

fn load_error(error: anyhow::Error) -> FuzzFailure {
    FuzzFailure::ReplayLoad {
        message: format!("{error:#}"),
    }
}
