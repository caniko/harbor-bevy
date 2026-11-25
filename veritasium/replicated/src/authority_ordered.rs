//! Server-authoritative passive consistency model.
//!
//! Re-baseline note: the shipped Wave 3 policy surface lives in `policy.rs`,
//! not a `consistency/` module. `ConsistencyModel` is sealed and currently
//! requires `OWNS_SCHEDULE`, `LABEL`, `Scheduler`, and `admit(&Transaction)`.
//! `CanonicalEventual` uses `PassiveScheduler` and admits every transaction.
//!
//! `AuthorityOrdered` keeps the authority concept local to this module. The
//! designated authority's `Transaction.sequence` is the canonical total order.
//! Non-authority transactions are proposals: they are drained from the receive
//! buffer and recorded in [`AuthorityOrderedProposals`], but they are not
//! applied to ECS state and are not folded into [`ReplicationChain`].
//! Consumers that want to accept a proposal route it to the authority, which
//! emits a new authority-authored transaction at the next canonical sequence.
//! The model is passive (`OWNS_SCHEDULE = false`) and does not drive the Bevy
//! schedule.

use bevy::prelude::Resource;
use std::sync::Arc;

use crate::policy::{
    ConsistencyModel, PassiveScheduler, ReliableOrdered, ReplicationEngine, sealed,
};
use crate::{NetworkId, OpaqueId, ReplicationPeerId, Transaction};

/// Passive authoritative consistency model.
pub struct AuthorityOrdered;

impl sealed::Sealed for AuthorityOrdered {}

impl ConsistencyModel for AuthorityOrdered {
    const OWNS_SCHEDULE: bool = false;
    const LABEL: &'static str = "AuthorityOrdered";

    type Scheduler = PassiveScheduler;
}

const _: () = assert!(!AuthorityOrdered::OWNS_SCHEDULE);

/// Default authoritative engine: reliable ordered delivery with authority order.
pub type AuthorityOrderedEngine = ReplicationEngine<ReliableOrdered, AuthorityOrdered>;

/// Model-local authority identity.
///
/// This is deliberately not part of `ReplicationParticipants`; equal-peer
/// models have no awareness of it.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AuthorityPeer {
    peer_id: ReplicationPeerId,
}

impl AuthorityPeer {
    pub const fn new(peer_id: ReplicationPeerId) -> Self {
        Self { peer_id }
    }

    pub const fn peer_id(self) -> ReplicationPeerId {
        self.peer_id
    }

    pub fn admits<Id: NetworkId>(self, transaction: &Transaction<Id>) -> bool {
        transaction.peer_id == self.peer_id
    }

    pub fn sequence_proposal<Id: NetworkId>(
        self,
        proposal: &Transaction<Id>,
        sequence: u64,
    ) -> Transaction<Id> {
        Transaction::new(self.peer_id, sequence, proposal.mutations.clone())
    }
}

/// Local participant role for examples and integrations.
///
/// The role is advisory; admission is based on [`AuthorityPeer`].
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthorityOrderedRole {
    Authority,
    #[default]
    Reader,
}

/// Non-authority transactions observed by an `AuthorityOrdered` engine.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct AuthorityOrderedProposals<Id: NetworkId = OpaqueId> {
    proposals: Vec<Arc<Transaction<Id>>>,
}

impl<Id: NetworkId> Default for AuthorityOrderedProposals<Id> {
    fn default() -> Self {
        Self {
            proposals: Vec::new(),
        }
    }
}

impl<Id: NetworkId> AuthorityOrderedProposals<Id> {
    pub fn proposals(&self) -> &[Arc<Transaction<Id>>] {
        &self.proposals
    }

    pub fn drain(&mut self) -> impl Iterator<Item = Arc<Transaction<Id>>> + '_ {
        self.proposals.drain(..)
    }

    pub(crate) fn record(&mut self, proposal: Arc<Transaction<Id>>) {
        self.proposals.push(proposal);
    }
}
