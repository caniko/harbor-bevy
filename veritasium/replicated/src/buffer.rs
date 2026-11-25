// intentionally oversize: release-gate baseline; split requires scoped semantic extraction.
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::prelude::Resource;

use crate::ordering::TransactionKey;
use crate::{
    Mutation, NetworkId, OpaqueId, ReplicationError, ReplicationParticipants, ReplicationPeerId,
    ReplicationResult, Transaction, WIRE_VERSION, WireError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BufferedTransaction<Id: NetworkId = OpaqueId> {
    pub transaction: Arc<Transaction<Id>>,
    pub wire_bytes: Vec<u8>,
}

impl<Id: NetworkId> BufferedTransaction<Id> {
    fn new(transaction: Arc<Transaction<Id>>, wire_bytes: Vec<u8>) -> Self {
        Self {
            transaction,
            wire_bytes,
        }
    }
}

type BufferedTransactionBatch<Id> = Vec<BufferedTransaction<Id>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveTransactionStatus {
    Buffered,
    DuplicateBuffered,
    DuplicateApplied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionGapAlert {
    pub peer_id: ReplicationPeerId,
    pub expected_sequence: u64,
    pub first_observed_sequence: u64,
    pub buffered_count: usize,
    pub blocked_by: Option<TransactionKey>,
}

#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapDetectionConfig {
    pub timeout: Duration,
}

impl Default for GapDetectionConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(200),
        }
    }
}

#[derive(Resource, Debug, Default)]
pub struct TransactionGapAlerts {
    alerts: Vec<TransactionGapAlert>,
}

impl TransactionGapAlerts {
    pub fn alerts(&self) -> &[TransactionGapAlert] {
        &self.alerts
    }

    pub fn clear(&mut self) {
        self.alerts.clear();
    }

    pub(crate) fn extend(&mut self, alerts: impl IntoIterator<Item = TransactionGapAlert>) {
        self.alerts.extend(alerts);
    }
}

#[derive(Debug, Clone)]
struct PendingGap {
    expected_sequence: u64,
    first_observed_sequence: u64,
    started_at: Instant,
    alerted: bool,
}

#[derive(Debug, Clone)]
struct PendingOrderingGap {
    blocked_by: TransactionKey,
    started_at: Instant,
    alerted: bool,
}

#[derive(Resource, Debug)]
pub struct TransactionBuffer<Id: NetworkId = OpaqueId> {
    peer_id: ReplicationPeerId,
    next_sequence: u64,
    active_batch: Option<Vec<Mutation<Id>>>,
    /// Structural mutations (insert/remove) captured by the lifecycle hooks during
    /// the current frame. Drained into a single committed batch by
    /// [`commit_structural_frame`](Self::commit_structural_frame) so that all
    /// structural changes a frame makes form one atomic transaction (e.g. a paired
    /// `DivineBlessing` + `Invulnerable` grant), independent of how many separate
    /// `entity_mut().insert()` / `.remove()` calls produced them.
    structural_frame: Vec<Mutation<Id>>,
    committed_batches: Vec<Vec<Mutation<Id>>>,
    outbound: VecDeque<Transaction<Id>>,
    per_peer_receive: HashMap<ReplicationPeerId, BTreeMap<u64, BufferedTransaction<Id>>>,
    next_expected: HashMap<ReplicationPeerId, u64>,
    applied: HashMap<ReplicationPeerId, BTreeMap<u64, BufferedTransaction<Id>>>,
    pending_gaps: HashMap<ReplicationPeerId, PendingGap>,
    pending_ordering_gaps: HashMap<TransactionKey, PendingOrderingGap>,
}

impl<Id: NetworkId> Default for TransactionBuffer<Id> {
    fn default() -> Self {
        Self {
            peer_id: ReplicationPeerId([0; 32]),
            next_sequence: 0,
            active_batch: None,
            structural_frame: Vec::new(),
            committed_batches: Vec::new(),
            outbound: VecDeque::new(),
            per_peer_receive: HashMap::new(),
            next_expected: HashMap::new(),
            applied: HashMap::new(),
            pending_gaps: HashMap::new(),
            pending_ordering_gaps: HashMap::new(),
        }
    }
}

impl<Id: NetworkId> TransactionBuffer<Id> {
    pub fn set_peer_id(&mut self, peer_id: ReplicationPeerId) {
        if self.peer_id != peer_id {
            self.peer_id = peer_id;
            self.next_sequence = self.next_expected_sequence(peer_id);
        }
    }

    /// Start a fresh fixed-participant replication session for `peer_id`.
    ///
    /// Gameplay may mutate local ECS state before a framework has completed
    /// peer negotiation. Those provisional transactions belong to no negotiated
    /// session and must not leak into the match transaction chain.
    pub fn reset_for_peer(&mut self, peer_id: ReplicationPeerId) {
        self.peer_id = peer_id;
        self.next_sequence = 0;
        self.active_batch = None;
        self.structural_frame.clear();
        self.committed_batches.clear();
        self.outbound.clear();
        self.per_peer_receive.clear();
        self.next_expected.clear();
        self.applied.clear();
        self.pending_gaps.clear();
        self.pending_ordering_gaps.clear();
    }

    pub fn peer_id(&self) -> ReplicationPeerId {
        self.peer_id
    }

    pub fn next_local_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub fn outbound(&self) -> &VecDeque<Transaction<Id>> {
        &self.outbound
    }

    pub fn drain_outbound(&mut self) -> std::collections::vec_deque::Drain<'_, Transaction<Id>> {
        self.outbound.drain(..)
    }

    pub fn begin_batch(&mut self) {
        assert!(
            self.active_batch.is_none(),
            "nested bevy_replicated batches are not supported"
        );
        self.active_batch = Some(Vec::new());
    }

    pub fn record(&mut self, mutation: Mutation<Id>) {
        if let Some(batch) = &mut self.active_batch {
            batch.push(mutation);
        } else {
            self.committed_batches.push(vec![mutation]);
        }
    }

    pub fn finish_batch(&mut self) {
        let batch = self
            .active_batch
            .take()
            .expect("finish_batch called without begin_batch");
        if !batch.is_empty() {
            self.committed_batches.push(batch);
        }
    }

    /// Records a structural mutation (insert/remove) captured by a lifecycle hook.
    ///
    /// Unlike [`record`](Self::record), structural records accumulate across the
    /// frame and are committed together by [`commit_structural_frame`](Self::commit_structural_frame),
    /// so a frame's structural changes form one atomic transaction regardless of
    /// how many individual world inserts/removes produced them.
    pub fn record_structural(&mut self, mutation: Mutation<Id>) {
        self.structural_frame.push(mutation);
    }

    /// Commits the frame's accumulated structural mutations as a single batch.
    ///
    /// Runs once per frame (in `PostUpdate`, before the flush). Committing after
    /// the frame's in-place mediated writes gives the structural transaction a
    /// later sequence than the same frame's modify transactions —
    /// structural inserts/removes target distinct (effect) components from the
    /// in-place modifies (state), so the ordering is independent.
    pub(crate) fn commit_structural_frame(&mut self) {
        if !self.structural_frame.is_empty() {
            let batch = std::mem::take(&mut self.structural_frame);
            self.committed_batches.push(batch);
        }
    }

    pub(crate) fn drain_committed_transactions(&mut self) -> Vec<Transaction<Id>> {
        assert!(
            self.active_batch.is_none(),
            "cannot flush while a bevy_replicated batch is active"
        );
        let batches = std::mem::take(&mut self.committed_batches);
        batches
            .into_iter()
            .map(|mutations| {
                let sequence = self.next_sequence;
                self.next_sequence += 1;
                Transaction::new(self.peer_id, sequence, mutations)
            })
            .collect()
    }

    pub(crate) fn enqueue_local_transaction(
        &mut self,
        transaction: Arc<Transaction<Id>>,
        wire_bytes: Vec<u8>,
    ) -> ReplicationResult<ReceiveTransactionStatus, Id> {
        self.outbound.push_back(transaction.as_ref().clone());
        self.receive_transaction_buffered(BufferedTransaction::new(transaction, wire_bytes))
    }

    pub fn receive_transaction(
        &mut self,
        transaction: Transaction<Id>,
    ) -> ReplicationResult<ReceiveTransactionStatus, Id> {
        self.receive_transaction_buffered(BufferedTransaction::new(
            Arc::new(transaction),
            Vec::new(),
        ))
    }

    pub(crate) fn receive_transaction_with_wire_bytes(
        &mut self,
        transaction: Transaction<Id>,
        wire_bytes: Vec<u8>,
    ) -> ReplicationResult<ReceiveTransactionStatus, Id> {
        self.receive_transaction_buffered(BufferedTransaction::new(
            Arc::new(transaction),
            wire_bytes,
        ))
    }

    fn receive_transaction_buffered(
        &mut self,
        buffered_transaction: BufferedTransaction<Id>,
    ) -> ReplicationResult<ReceiveTransactionStatus, Id> {
        let transaction = &buffered_transaction.transaction;
        if transaction.version != WIRE_VERSION {
            return Err(ReplicationError::Wire(WireError::Version {
                expected: WIRE_VERSION,
                found: transaction.version,
            }));
        }

        let peer_id = transaction.peer_id;
        let sequence = transaction.sequence;
        if let Some(applied) = self
            .applied
            .get(&peer_id)
            .and_then(|transactions| transactions.get(&sequence))
        {
            if applied.transaction == buffered_transaction.transaction {
                return Ok(ReceiveTransactionStatus::DuplicateApplied);
            }
            return Err(ReplicationError::ConflictingTransactionKey {
                peer_id,
                sequence,
                existing_state: "applied",
                existing_mutations: applied.transaction.mutations.len(),
                incoming_mutations: transaction.mutations.len(),
            });
        }

        if let Some(buffered) = self
            .per_peer_receive
            .get(&peer_id)
            .and_then(|transactions| transactions.get(&sequence))
        {
            if buffered.transaction == buffered_transaction.transaction {
                return Ok(ReceiveTransactionStatus::DuplicateBuffered);
            }
            return Err(ReplicationError::ConflictingTransactionKey {
                peer_id,
                sequence,
                existing_state: "buffered",
                existing_mutations: buffered.transaction.mutations.len(),
                incoming_mutations: transaction.mutations.len(),
            });
        }

        self.per_peer_receive
            .entry(peer_id)
            .or_default()
            .insert(sequence, buffered_transaction);
        self.refresh_gap(peer_id, Instant::now());
        Ok(ReceiveTransactionStatus::Buffered)
    }

    pub(crate) fn drain_ready_transactions(
        &mut self,
        participants: &ReplicationParticipants,
    ) -> BufferedTransactionBatch<Id> {
        let mut ready = Vec::new();
        while let Some(transaction) = self.pop_next_ready_transaction(participants) {
            ready.push(transaction);
        }
        ready
    }

    pub(crate) fn drain_authority_ordered_transactions(
        &mut self,
        authority_peer: ReplicationPeerId,
    ) -> (BufferedTransactionBatch<Id>, Vec<Arc<Transaction<Id>>>) {
        let mut authority_ordered = Vec::new();
        while let Some(transaction) = self.pop_next_ready_transaction_from(authority_peer) {
            authority_ordered.push(transaction);
        }

        let mut proposal_keys = Vec::new();
        for (peer_id, transactions) in &self.per_peer_receive {
            if *peer_id == authority_peer {
                continue;
            }
            proposal_keys.extend(
                transactions
                    .keys()
                    .copied()
                    .map(|sequence| TransactionKey::new(*peer_id, sequence)),
            );
        }
        proposal_keys.sort();

        let mut proposals = Vec::new();
        for key in proposal_keys {
            if let Some(transaction) = self.remove_buffered_transaction(key) {
                self.mark_applied(key, transaction.clone());
                proposals.push(transaction.transaction);
            }
        }

        (authority_ordered, proposals)
    }

    pub(crate) fn detect_gap_alerts(
        &mut self,
        timeout: Duration,
        participants: &ReplicationParticipants,
    ) -> Vec<TransactionGapAlert> {
        let now = Instant::now();
        self.refresh_all_gaps(now);
        self.refresh_ordering_gaps(participants, now);

        let mut alerts = Vec::new();
        for (peer_id, gap) in &mut self.pending_gaps {
            if gap.alerted || now.duration_since(gap.started_at) < timeout {
                continue;
            }
            let buffered_count = self.per_peer_receive.get(peer_id).map_or(0, BTreeMap::len);
            alerts.push(TransactionGapAlert {
                peer_id: *peer_id,
                expected_sequence: gap.expected_sequence,
                first_observed_sequence: gap.first_observed_sequence,
                buffered_count,
                blocked_by: None,
            });
            gap.alerted = true;
        }
        for (missing, gap) in &mut self.pending_ordering_gaps {
            if gap.alerted || now.duration_since(gap.started_at) < timeout {
                continue;
            }
            let buffered_count = self
                .per_peer_receive
                .get(&missing.peer_id)
                .map_or(0, BTreeMap::len);
            alerts.push(TransactionGapAlert {
                peer_id: missing.peer_id,
                expected_sequence: missing.sequence,
                first_observed_sequence: gap.blocked_by.sequence,
                buffered_count,
                blocked_by: Some(gap.blocked_by),
            });
            gap.alerted = true;
        }
        alerts
    }

    fn pop_next_ready_transaction(
        &mut self,
        participants: &ReplicationParticipants,
    ) -> Option<BufferedTransaction<Id>> {
        let active_peers = self.active_peers(participants);
        let key = self
            .per_peer_receive
            .iter()
            .filter_map(|(peer_id, transactions)| {
                let expected = self.next_expected_sequence(*peer_id);
                transactions
                    .contains_key(&expected)
                    .then_some(TransactionKey::new(*peer_id, expected))
            })
            .filter(|candidate| {
                self.first_missing_lower_key(*candidate, &active_peers, participants)
                    .is_none()
            })
            .min()?;

        let transaction = self
            .remove_buffered_transaction(key)
            .expect("ready transaction should be present");
        self.mark_applied(key, transaction.clone());
        Some(transaction)
    }

    fn pop_next_ready_transaction_from(
        &mut self,
        peer_id: ReplicationPeerId,
    ) -> Option<BufferedTransaction<Id>> {
        let key = TransactionKey::new(peer_id, self.next_expected_sequence(peer_id));
        let transaction = self.remove_buffered_transaction(key)?;
        self.mark_applied(key, transaction.clone());
        Some(transaction)
    }

    fn remove_buffered_transaction(
        &mut self,
        key: TransactionKey,
    ) -> Option<BufferedTransaction<Id>> {
        let transaction = self
            .per_peer_receive
            .get_mut(&key.peer_id)
            .and_then(|transactions| transactions.remove(&key.sequence))?;
        if self
            .per_peer_receive
            .get(&key.peer_id)
            .is_some_and(BTreeMap::is_empty)
        {
            self.per_peer_receive.remove(&key.peer_id);
        }
        Some(transaction)
    }

    fn mark_applied(&mut self, key: TransactionKey, transaction: BufferedTransaction<Id>) {
        let next_sequence = key.sequence.saturating_add(1);
        let current_next = self.next_expected_sequence(key.peer_id);
        self.next_expected
            .insert(key.peer_id, current_next.max(next_sequence));
        self.applied
            .entry(key.peer_id)
            .or_default()
            .insert(key.sequence, transaction);
        self.refresh_gap(key.peer_id, Instant::now());
    }

    fn for_each_ready_candidate_key(&self, mut f: impl FnMut(TransactionKey)) {
        self.per_peer_receive
            .iter()
            .for_each(|(peer_id, transactions)| {
                let expected = self.next_expected_sequence(*peer_id);
                if transactions.contains_key(&expected) {
                    f(TransactionKey::new(*peer_id, expected));
                }
            });
    }

    fn active_peers(&self, participants: &ReplicationParticipants) -> BTreeSet<ReplicationPeerId> {
        let mut peers: BTreeSet<_> = participants.peers().collect();
        if participants.is_dynamic_discovery() {
            peers.extend(self.per_peer_receive.keys().copied());
            peers.extend(self.applied.keys().copied());
            peers.extend(self.next_expected.keys().copied());
        }
        peers
    }

    fn first_missing_lower_key(
        &self,
        candidate: TransactionKey,
        active_peers: &BTreeSet<ReplicationPeerId>,
        participants: &ReplicationParticipants,
    ) -> Option<TransactionKey> {
        active_peers
            .iter()
            .copied()
            .filter_map(|peer_id| {
                let key = TransactionKey::new(peer_id, self.next_expected_sequence(peer_id));
                if key >= candidate || self.has_ready_transaction(key) {
                    return None;
                }
                if participants.is_dynamic_discovery()
                    && !self.peer_has_buffered_transaction_after(peer_id, key.sequence)
                {
                    return None;
                }
                Some(key)
            })
            .min()
    }

    fn has_ready_transaction(&self, key: TransactionKey) -> bool {
        self.per_peer_receive
            .get(&key.peer_id)
            .is_some_and(|transactions| transactions.contains_key(&key.sequence))
    }

    fn peer_has_buffered_transaction_after(
        &self,
        peer_id: ReplicationPeerId,
        sequence: u64,
    ) -> bool {
        self.per_peer_receive
            .get(&peer_id)
            .is_some_and(|transactions| transactions.keys().any(|other| *other > sequence))
    }

    fn next_expected_sequence(&self, peer_id: ReplicationPeerId) -> u64 {
        self.next_expected.get(&peer_id).copied().unwrap_or(0)
    }

    fn refresh_all_gaps(&mut self, now: Instant) {
        let peers: Vec<_> = self.per_peer_receive.keys().copied().collect();
        for peer_id in peers {
            self.refresh_gap_at(peer_id, now);
        }

        let cleared: Vec<_> = self
            .pending_gaps
            .keys()
            .copied()
            .filter(|peer_id| !self.per_peer_receive.contains_key(peer_id))
            .collect();
        for peer_id in cleared {
            self.pending_gaps.remove(&peer_id);
        }
    }

    fn refresh_ordering_gaps(&mut self, participants: &ReplicationParticipants, now: Instant) {
        let active_peers = self.active_peers(participants);
        let mut current_gaps = HashMap::new();
        self.for_each_ready_candidate_key(|candidate| {
            if let Some(missing) =
                self.first_missing_lower_key(candidate, &active_peers, participants)
            {
                current_gaps
                    .entry(missing)
                    .and_modify(|blocked_by: &mut TransactionKey| {
                        if candidate < *blocked_by {
                            *blocked_by = candidate;
                        }
                    })
                    .or_insert(candidate);
            }
        });

        self.pending_ordering_gaps
            .retain(|missing, _| current_gaps.contains_key(missing));

        for (missing, blocked_by) in current_gaps {
            match self.pending_ordering_gaps.get_mut(&missing) {
                Some(gap) if gap.blocked_by == blocked_by => {}
                Some(gap) => {
                    *gap = PendingOrderingGap {
                        blocked_by,
                        started_at: now,
                        alerted: false,
                    };
                }
                None => {
                    self.pending_ordering_gaps.insert(
                        missing,
                        PendingOrderingGap {
                            blocked_by,
                            started_at: now,
                            alerted: false,
                        },
                    );
                }
            }
        }
    }

    fn refresh_gap(&mut self, peer_id: ReplicationPeerId, now: Instant) {
        self.refresh_gap_at(peer_id, now);
    }

    fn refresh_gap_at(&mut self, peer_id: ReplicationPeerId, now: Instant) {
        let expected = self.next_expected_sequence(peer_id);
        let first_observed = self
            .per_peer_receive
            .get(&peer_id)
            .and_then(|transactions| transactions.keys().next().copied());

        let Some(first_observed_sequence) = first_observed else {
            self.pending_gaps.remove(&peer_id);
            return;
        };

        if first_observed_sequence <= expected {
            self.pending_gaps.remove(&peer_id);
            return;
        }

        match self.pending_gaps.get_mut(&peer_id) {
            Some(gap)
                if gap.expected_sequence == expected
                    && gap.first_observed_sequence == first_observed_sequence => {}
            Some(gap) => {
                *gap = PendingGap {
                    expected_sequence: expected,
                    first_observed_sequence,
                    started_at: now,
                    alerted: false,
                };
            }
            None => {
                self.pending_gaps.insert(
                    peer_id,
                    PendingGap {
                        expected_sequence: expected,
                        first_observed_sequence,
                        started_at: now,
                        alerted: false,
                    },
                );
            }
        }
    }
}
