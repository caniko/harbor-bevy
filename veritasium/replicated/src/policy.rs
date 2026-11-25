//! Compile-time replication policy axes.
//!
//! `OrderingPolicy` and `ConsistencyModel` use associated-const policy traits, ZST marker
//! implementations, `PhantomData` carriers, and concrete resource aliases.
//! They are consumed as generic bounds, never as trait objects.
//!
//! Sealing policy:
//! - Sealed: `OrderingPolicy` and `ConsistencyModel`. These are
//!   determinism-critical surfaces tied to wire formats, ordering, chain
//!   hashing, and Bevy resource registration.
//!
//! The full clock-ownership surface is reserved here for Wave 4. The
//! `OWNS_SCHEDULE` const selects the plugin branch, while `Scheduler` provides
//! what an owning model calls and `admit` provides the live admission hook.
//! Reserving these now means a future rollback model adds an impl, not a trait
//! method. The passive shipped models are `CanonicalEventual` and
//! `AuthorityOrdered`.

use std::marker::PhantomData;

use bevy::prelude::*;

use crate::{NetworkId, Transaction};

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// Per-channel ordering discipline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    ReliableOrdered,
    ReliableUnordered,
    Unreliable,
    TickBuffered,
}

/// Compile-time policy for transaction ordering and transport channel shape.
pub trait OrderingPolicy: sealed::Sealed + Send + Sync + 'static {
    const CHANNEL_KIND: ChannelKind;
    const LABEL: &'static str;
}

/// Current reliable, per-peer-sequence channel discipline.
pub struct ReliableOrdered;

impl sealed::Sealed for ReliableOrdered {}

impl OrderingPolicy for ReliableOrdered {
    const CHANNEL_KIND: ChannelKind = ChannelKind::ReliableOrdered;
    const LABEL: &'static str = "ReliableOrdered";
}

/// Zero-sized carrier for ordering policy type parameters.
pub type OrderingMarker<O> = PhantomData<O>;

/// What a schedule-owning consistency model drives.
///
/// `PassiveScheduler` is the no-op default. A future rollback model supplies a
/// real driver by overriding `ConsistencyModel::Scheduler`.
pub trait ScheduleDriver: Send + Sync + 'static {
    fn build(_app: &mut App) {}
}

pub struct PassiveScheduler;

impl ScheduleDriver for PassiveScheduler {}

/// Compile-time policy for mutation convergence.
pub trait ConsistencyModel: sealed::Sealed + Send + Sync + 'static {
    /// Selects whether the plugin registers a model-owned schedule driver or
    /// the passive apply/flush systems.
    const OWNS_SCHEDULE: bool;
    const LABEL: &'static str;

    /// Reserved clock-ownership surface. Passive models use the no-op driver.
    type Scheduler: ScheduleDriver;

    /// Live admission hook for ready transactions.
    fn admit<Id: NetworkId>(_txn: &Transaction<Id>) -> bool {
        true
    }
}

/// The current canonical-order plus chain-hash convergence model.
///
/// This names the behavior `bevy_replicated` already implements: transactions
/// are applied in canonical `(sequence, peer_id)` order and folded into the
/// replication chain. It is passive and admits every ready transaction.
pub struct CanonicalEventual;

impl sealed::Sealed for CanonicalEventual {}

impl ConsistencyModel for CanonicalEventual {
    const OWNS_SCHEDULE: bool = false;
    const LABEL: &'static str = "CanonicalEventual";

    type Scheduler = PassiveScheduler;
}

const _: () = assert!(!CanonicalEventual::OWNS_SCHEDULE);

/// Zero-sized carrier for consistency model type parameters.
pub type ConsistencyMarker<C> = PhantomData<C>;

/// Combined compile-time replication engine policy.
#[derive(Resource)]
pub struct ReplicationEngine<
    O: OrderingPolicy = ReliableOrdered,
    C: ConsistencyModel = CanonicalEventual,
> {
    marker: PhantomData<(O, C)>,
}

impl<O: OrderingPolicy, C: ConsistencyModel> Default for ReplicationEngine<O, C> {
    fn default() -> Self {
        Self {
            marker: PhantomData,
        }
    }
}

/// Default engine: reliable ordered delivery with canonical eventual apply.
pub type CanonicalEventualEngine = ReplicationEngine<ReliableOrdered, CanonicalEventual>;

// TODO(wave3-03): canonical Transport / FailureDetector traits land here as
// part of the sealed surface once their semantics are pinned.
