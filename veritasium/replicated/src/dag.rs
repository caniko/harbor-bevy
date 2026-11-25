//! Thread-safe DAG validator handles shared between actor threads and the
//! Bevy main thread.
//!
//! Each DAG validator `D` is wrapped in `Arc<Mutex<D>>`; the Bevy side
//! accesses it via [`DagResource<D>`]. System/observer factories that fire
//! game-specific violation events stay in the game layer — this module
//! owns only the handle and lock discipline.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;

/// Thread-safe handle to a DAG validator, shared between actor + Bevy threads.
///
/// Monomorphized per validator type: `DagHandle<ActionDag>`,
/// `DagHandle<TournamentDag>`, etc.
#[derive(Debug)]
pub struct DagHandle<D>(pub Arc<Mutex<D>>);

impl<D> Clone for DagHandle<D> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<D: Default> Default for DagHandle<D> {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(D::default())))
    }
}

impl<D: Default> DagHandle<D> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<D> DagHandle<D> {
    pub fn with(dag: D) -> Self {
        Self(Arc::new(Mutex::new(dag)))
    }
}

/// Bevy resource wrapping a [`DagHandle`].
///
/// Monomorphized per validator type so each DAG gets its own resource slot.
#[derive(Resource)]
pub struct DagResource<D: Send + Sync + 'static>(pub DagHandle<D>);

impl<D: Send + Sync + 'static> Clone for DagResource<D> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<D: Send + Sync + 'static> DagResource<D> {
    /// Lock the DAG for mutation. Recovers from poisoned mutex.
    pub fn lock(&self) -> std::sync::MutexGuard<'_, D> {
        match self.0.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                log::error!("DAG mutex was poisoned — recovering");
                poisoned.into_inner()
            }
        }
    }
}
