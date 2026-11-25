// intentionally oversize: release-gate baseline; split requires scoped semantic extraction.
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::mutation_log::{LogEntry, MutationLog, MutationLogSet};
use bevy::prelude::*;

use crate::transaction::{ReplicationIdentity, ReplicationPeerId};
use crate::wire::{TransactionWireId, WireError, encode_chain_hash_report, encode_transaction};
use crate::{NetworkId, ReplicationResult, Transaction};

/// Version mixed into the replication chain genesis hash.
///
/// Bump this only when peers with the previous chain domain must be rejected
/// rather than compared, for example after changing canonical transaction bytes.
pub const CHAIN_DOMAIN_VERSION: u32 = 3;
/// Domain separator mixed into every replication chain.
pub const CHAIN_DOMAIN_TAG: &[u8] = b"bevy_replicated.transaction_chain.v3";
const CHAIN_ENTRY_TAG: &[u8] = b"entry";

/// Fixed-width digest used for replication chain checkpoints.
pub type ChainHash = [u8; 32];

/// Retained transaction-chain history capacity, in applied entries.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainHistoryCapacity(pub usize);

impl Default for ChainHistoryCapacity {
    fn default() -> Self {
        Self(65_536)
    }
}

/// Requested chain history has already fallen out of the retained window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryEvicted {
    /// Checkpoint count requested by the remote peer or verifier.
    pub requested_count: u64,
    /// Oldest checkpoint count still retained locally.
    pub oldest_available: u64,
}

/// Mutation-log set used by `bevy_replicated`.
///
/// This is the library's manual stand-in for the RFC `MutationLogSet` derive.
/// `ReplicationChain` keeps the existing public reporting surface, while the
/// rolling hash and checkpoint history live in `MutationLog<ReplicatedSet>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicatedSet;

impl MutationLogSet for ReplicatedSet {
    const DOMAIN_TAG: &'static [u8] = CHAIN_DOMAIN_TAG;
    const DOMAIN_VERSION: u32 = CHAIN_DOMAIN_VERSION;
    const ENTRY_TAG: &'static [u8] = CHAIN_ENTRY_TAG;
}

/// Hash function used for the replication chain digest.
///
/// The selected algorithm is part of the genesis domain. Mixed algorithms
/// produce different chains even for identical transactions.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
pub enum HashAlgorithm {
    #[default]
    Blake3,
    Sha256,
}

/// Rolling hash of canonical replicated transactions applied by this app.
///
/// The chain is deterministic across equal peers only when they apply the same
/// canonical transaction bytes in the same order. Divergence means a protocol,
/// replication, or game-state bug; consumers should not repair it by resetting
/// the chain.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct ReplicationChain {
    algorithm: HashAlgorithm,
    log: MutationLog<ReplicatedSet>,
    applied_count: u64,
}

impl ReplicationChain {
    /// Create a chain with the default hash algorithm and history capacity.
    #[must_use]
    pub fn new() -> Self {
        Self::with_algorithm(HashAlgorithm::Blake3)
    }

    /// Create a chain with a specific hash algorithm.
    #[must_use]
    pub fn with_algorithm(algorithm: HashAlgorithm) -> Self {
        Self::with_algorithm_and_history_capacity(algorithm, ChainHistoryCapacity::default())
    }

    /// Create a chain with the default hash algorithm and custom history.
    #[must_use]
    pub fn with_history_capacity(capacity: ChainHistoryCapacity) -> Self {
        Self::with_algorithm_and_history_capacity(HashAlgorithm::Blake3, capacity)
    }

    /// Create a chain with explicit algorithm and retained history capacity.
    #[must_use]
    pub fn with_algorithm_and_history_capacity(
        algorithm: HashAlgorithm,
        capacity: ChainHistoryCapacity,
    ) -> Self {
        let current_hash = digest(
            algorithm,
            &[
                CHAIN_DOMAIN_TAG,
                &CHAIN_DOMAIN_VERSION.to_le_bytes(),
                algorithm.domain_bytes(),
            ],
        );
        Self {
            algorithm,
            log: MutationLog::with_genesis_hash_and_capacity(current_hash, capacity.0),
            applied_count: 0,
        }
    }

    /// Hash algorithm mixed into this chain's domain.
    #[must_use]
    pub fn algorithm(&self) -> HashAlgorithm {
        self.algorithm
    }

    /// Current replication chain hash.
    #[must_use]
    pub fn current(&self) -> ChainHash {
        self.log.current_hash()
    }

    /// Number of transactions folded into this chain.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.applied_count
    }

    /// Number of transactions folded into this chain.
    #[must_use]
    pub fn applied_count(&self) -> u64 {
        self.applied_count
    }

    /// Return the checkpoint hash at an applied transaction count, if retained.
    #[must_use]
    pub fn at_count(&self, count: u64) -> Option<ChainHash> {
        self.log.hash_at_count(count)
    }

    /// Return a retained checkpoint or explain that it has been evicted.
    pub fn at_count_or_evicted(&self, count: u64) -> Result<Option<ChainHash>, HistoryEvicted> {
        if count < self.oldest_available_count() {
            return Err(HistoryEvicted {
                requested_count: count,
                oldest_available: self.oldest_available_count(),
            });
        }
        Ok(self.at_count(count))
    }

    /// Oldest applied transaction count still available for comparison.
    #[must_use]
    pub fn oldest_available_count(&self) -> u64 {
        self.log.oldest_available_count()
    }

    /// Fold one canonical transaction into the chain.
    ///
    /// The transaction is encoded through the public wire format first, so chain
    /// hashes compare transport-visible bytes rather than private Rust layout.
    pub fn fold<Id: TransactionWireId>(
        &mut self,
        transaction: &Transaction<Id>,
    ) -> Result<(), WireError> {
        let wire_bytes = encode_transaction(transaction)?;
        self.fold_canonical_bytes(transaction, &wire_bytes);
        Ok(())
    }

    pub(crate) fn fold_canonical_bytes<Id: NetworkId>(
        &mut self,
        transaction: &Transaction<Id>,
        wire_bytes: &[u8],
    ) {
        let next_hash = digest(
            self.algorithm,
            &[
                &self.log.current_hash(),
                CHAIN_ENTRY_TAG,
                &transaction.peer_id.0,
                &transaction.sequence.to_le_bytes(),
                &(wire_bytes.len() as u64).to_le_bytes(),
                wire_bytes,
            ],
        );
        self.applied_count += 1;
        self.log
            .append_with_hash(LogEntry::canonical_bytes(wire_bytes), next_hash);
    }
}

impl Default for ReplicationChain {
    fn default() -> Self {
        Self::new()
    }
}

impl HashAlgorithm {
    fn domain_bytes(self) -> &'static [u8] {
        match self {
            Self::Blake3 => b"blake3",
            Self::Sha256 => b"sha256",
        }
    }
}

fn digest(algorithm: HashAlgorithm, parts: &[&[u8]]) -> ChainHash {
    match algorithm {
        HashAlgorithm::Blake3 => {
            let mut hasher = blake3::Hasher::new();
            for part in parts {
                hasher.update(part);
            }
            hasher.finalize().into()
        }
        HashAlgorithm::Sha256 => {
            use sha2::Digest as _;

            let mut hasher = sha2::Sha256::new();
            for part in parts {
                hasher.update(part);
            }
            hasher.finalize().into()
        }
    }
}

/// Chain checkpoint exchanged between peers for divergence detection.
///
/// A `Hash` report says "after N applied transactions my chain digest is H".
/// `HistoryEvicted` is a loud diagnostic path for a peer requesting a checkpoint
/// older than the retained local history.
#[derive(
    Event, Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
pub enum ChainHashReport {
    /// Checkpoint hash at an applied transaction count.
    Hash {
        /// Peer that produced this checkpoint.
        peer: ReplicationPeerId,
        /// Number of canonical transactions folded before computing `hash`.
        applied_count: u64,
        /// Chain digest at `applied_count`.
        hash: ChainHash,
    },
    /// Requested checkpoint is no longer retained by the reporting peer.
    HistoryEvicted {
        /// Peer that could not provide the requested checkpoint.
        peer: ReplicationPeerId,
        /// Applied count requested by the verifier.
        requested_count: u64,
        /// Oldest retained count on the reporting peer.
        oldest_available: u64,
    },
}

impl ChainHashReport {
    /// Create a hash checkpoint report.
    #[must_use]
    pub fn new(peer: ReplicationPeerId, applied_count: u64, hash: ChainHash) -> Self {
        Self::Hash {
            peer,
            applied_count,
            hash,
        }
    }

    /// Peer that produced this report.
    #[must_use]
    pub fn peer(&self) -> ReplicationPeerId {
        match *self {
            Self::Hash { peer, .. } | Self::HistoryEvicted { peer, .. } => peer,
        }
    }

    /// Applied transaction count for hash reports.
    #[must_use]
    pub fn applied_count(&self) -> Option<u64> {
        match *self {
            Self::Hash { applied_count, .. } => Some(applied_count),
            Self::HistoryEvicted { .. } => None,
        }
    }

    /// Chain hash for hash reports.
    #[must_use]
    pub fn hash(&self) -> Option<ChainHash> {
        match *self {
            Self::Hash { hash, .. } => Some(hash),
            Self::HistoryEvicted { .. } => None,
        }
    }
}

/// Local event emitted when a remote chain checkpoint disagrees with ours.
#[derive(Event, Message, Debug, Clone, PartialEq, Eq)]
pub struct DivergenceEvent {
    /// Local chain hash at the compared applied count.
    pub local_hash: ChainHash,
    /// Remote peer's reported chain hash at the compared applied count.
    pub remote_hash: ChainHash,
    /// Peer that reported the mismatching hash.
    pub remote_peer: ReplicationPeerId,
    /// Applied transaction count where the mismatch was detected.
    pub applied_count_at_check: u64,
    /// Latest earlier count known to have matched, if any.
    pub last_common_count: Option<u64>,
}

impl DivergenceEvent {
    /// Build a concise human-readable divergence diagnostic.
    #[must_use]
    pub fn diagnostic_message(&self) -> String {
        format!(
            "bevy_replicated chain divergence from peer {:?} at applied_count={} (last_common={:?}, local={}, remote={})",
            self.remote_peer,
            self.applied_count_at_check,
            self.last_common_count,
            format_hash_short(&self.local_hash),
            format_hash_short(&self.remote_hash)
        )
    }
}

/// Format a chain hash for short diagnostic log messages.
#[must_use]
pub fn format_hash_short(hash: &ChainHash) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}...",
        hash[0], hash[1], hash[2], hash[3]
    )
}

/// Message requesting a phase-boundary chain hash report.
#[derive(Message, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChainHashCheckpoint;

/// Cadence and phase-boundary controls for chain-hash exchange.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainHashConfig {
    /// Emit a report every N applied transactions; zero disables cadence reports.
    pub transaction_cadence: u64,
    /// Emit a report when a [`ChainHashCheckpoint`] message is received.
    pub report_on_phase_boundary: bool,
}

impl Default for ChainHashConfig {
    fn default() -> Self {
        Self {
            transaction_cadence: 64,
            report_on_phase_boundary: true,
        }
    }
}

/// Transport-ready chain-hash report plus its canonical wire bytes.
#[derive(Debug, Clone)]
pub struct ChainHashEnvelope {
    /// Decoded report for local inspection and tests.
    pub report: ChainHashReport,
    /// Encoded report bytes suitable for network transport.
    pub wire_bytes: Vec<u8>,
}

/// Queue of chain-hash reports waiting for the integration layer to send.
#[derive(Resource, Debug, Default)]
pub struct ChainHashOutbox {
    envelopes: Vec<ChainHashEnvelope>,
}

impl ChainHashOutbox {
    /// Borrow queued chain-hash envelopes without clearing them.
    #[must_use]
    pub fn envelopes(&self) -> &[ChainHashEnvelope] {
        &self.envelopes
    }

    /// Remove and return all queued chain-hash envelopes.
    ///
    /// Callers may intentionally ignore the returned vector when they only need
    /// to clear the queue between test assertions.
    pub fn drain(&mut self) -> Vec<ChainHashEnvelope> {
        std::mem::take(&mut self.envelopes)
    }

    fn push(&mut self, envelope: ChainHashEnvelope) {
        self.envelopes.push(envelope);
    }
}

#[derive(Resource, Debug, Default)]
pub(crate) struct ChainHashExchangeState {
    last_reported_count: Option<u64>,
}

#[derive(Resource, Debug, Default)]
pub(crate) struct RemoteChainHashReports {
    last_common: HashMap<ReplicationPeerId, u64>,
    pending: BTreeMap<u64, Vec<ChainHashReport>>,
    reported_mismatches: HashSet<(ReplicationPeerId, u64)>,
}

/// Outcome of processing an inbound chain-hash report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainHashReportStatus {
    /// Remote checkpoint matched the local hash at the same applied count.
    Matched,
    /// Remote checkpoint disagreed with the local hash.
    Mismatched,
    /// Report was stored until the local chain reaches the reported count.
    PendingLocalCatchUp,
    /// Remote peer no longer has the requested historical checkpoint.
    HistoryEvicted,
    /// Report came from this local peer and was ignored.
    IgnoredSelfReport,
}

pub(crate) fn emit_due_chain_hash_report(world: &mut World) -> Result<(), WireError> {
    let peer_id = world.resource::<ReplicationIdentity>().peer_id;
    let chain = world.resource::<ReplicationChain>();
    let count = chain.applied_count();
    let cadence = world.resource::<ChainHashConfig>().transaction_cadence;
    if count == 0 || cadence == 0 {
        return Ok(());
    }

    let mut reports = Vec::new();
    let last_reported_count = world
        .resource::<ChainHashExchangeState>()
        .last_reported_count
        .unwrap_or(0);
    let mut next_report_count = last_reported_count.saturating_add(cadence);
    while next_report_count <= count {
        match chain.at_count_or_evicted(next_report_count) {
            Ok(Some(hash)) => reports.push(ChainHashReport::new(peer_id, next_report_count, hash)),
            Err(evicted) => {
                reports.push(ChainHashReport::HistoryEvicted {
                    peer: peer_id,
                    requested_count: evicted.requested_count,
                    oldest_available: evicted.oldest_available,
                });
                next_report_count = evicted.oldest_available;
                if next_report_count > count {
                    break;
                }
                continue;
            }
            Ok(None) => {
                return Err(WireError::InvariantBroken {
                    detail: format!(
                        "missing chain hash at count {next_report_count} while applied_count is {count}"
                    ),
                });
            }
        }
        next_report_count = next_report_count.saturating_add(cadence);
        if next_report_count == u64::MAX {
            break;
        }
    }

    if reports.is_empty() {
        return Ok(());
    }

    for report in reports {
        emit_report(world, report)?;
    }
    Ok(())
}

pub(crate) fn process_chain_hash_checkpoints(
    mut checkpoints: MessageReader<ChainHashCheckpoint>,
    config: Res<ChainHashConfig>,
    mut state: ResMut<ChainHashExchangeState>,
    identity: Res<ReplicationIdentity>,
    chain: Res<ReplicationChain>,
    mut outbox: ResMut<ChainHashOutbox>,
) {
    if !config.report_on_phase_boundary || checkpoints.read().next().is_none() {
        return;
    }
    if let Err(err) = push_report(
        &mut state,
        &mut outbox,
        ChainHashReport::new(identity.peer_id, chain.applied_count(), chain.current()),
    ) {
        log::warn!("chain hash report push failed: {err}");
    }
}

/// Queue an immediate chain-hash report for the current local count.
pub fn request_chain_hash_report(world: &mut World) -> ReplicationResult<()> {
    emit_chain_hash_report(world)?;
    Ok(())
}

/// Compare an inbound chain-hash report against local history.
///
/// Matching reports update the last-common checkpoint. Mismatches emit both a
/// Bevy event and message so observers and message readers can react.
#[must_use]
pub fn receive_chain_hash_report(
    world: &mut World,
    report: ChainHashReport,
) -> ChainHashReportStatus {
    let local_peer = world.resource::<ReplicationIdentity>().peer_id;
    if report.peer() == local_peer {
        return ChainHashReportStatus::IgnoredSelfReport;
    }

    compare_or_store_report(world, report)
}

pub(crate) fn process_pending_chain_hash_reports(world: &mut World) {
    let current_count = world.resource::<ReplicationChain>().applied_count();
    let ready_counts = {
        let reports = world.resource::<RemoteChainHashReports>();
        reports
            .pending
            .range(..=current_count)
            .map(|(count, _)| *count)
            .collect::<Vec<_>>()
    };

    for count in ready_counts {
        let reports = world
            .resource_mut::<RemoteChainHashReports>()
            .pending
            .remove(&count)
            .unwrap_or_default();
        for report in reports {
            let _ = compare_or_store_report(world, report);
        }
    }
}

fn emit_chain_hash_report(world: &mut World) -> Result<(), WireError> {
    let report = {
        let identity = world.resource::<ReplicationIdentity>();
        let chain = world.resource::<ReplicationChain>();
        ChainHashReport::new(identity.peer_id, chain.applied_count(), chain.current())
    };
    emit_report(world, report)
}

fn emit_report(world: &mut World, report: ChainHashReport) -> Result<(), WireError> {
    let last_reported_count = report.applied_count();
    let envelope = ChainHashEnvelope {
        wire_bytes: encode_chain_hash_report(&report)?,
        report,
    };
    world.resource_mut::<ChainHashOutbox>().push(envelope);
    if let Some(applied_count) = last_reported_count {
        world
            .resource_mut::<ChainHashExchangeState>()
            .last_reported_count = Some(applied_count);
    }
    Ok(())
}

pub(crate) fn push_report(
    state: &mut ChainHashExchangeState,
    outbox: &mut ChainHashOutbox,
    report: ChainHashReport,
) -> Result<(), WireError> {
    let last_reported_count = report.applied_count();
    let envelope = ChainHashEnvelope {
        wire_bytes: encode_chain_hash_report(&report)?,
        report,
    };
    outbox.push(envelope);
    if let Some(applied_count) = last_reported_count {
        state.last_reported_count = Some(applied_count);
    }
    Ok(())
}

fn compare_or_store_report(world: &mut World, report: ChainHashReport) -> ChainHashReportStatus {
    let ChainHashReport::Hash {
        peer,
        applied_count,
        hash,
    } = report
    else {
        world.trigger(report);
        return ChainHashReportStatus::HistoryEvicted;
    };

    let Some(local_hash) = world.resource::<ReplicationChain>().at_count(applied_count) else {
        world
            .resource_mut::<RemoteChainHashReports>()
            .pending
            .entry(applied_count)
            .or_default()
            .push(report);
        return ChainHashReportStatus::PendingLocalCatchUp;
    };

    if local_hash == hash {
        let mut reports = world.resource_mut::<RemoteChainHashReports>();
        reports.last_common.insert(peer, applied_count);
        reports.reported_mismatches.remove(&(peer, applied_count));
        return ChainHashReportStatus::Matched;
    }

    let (last_common_count, first_report_for_checkpoint) = {
        let mut reports = world.resource_mut::<RemoteChainHashReports>();
        let last_common_count = reports.last_common.get(&peer).copied();
        let first_report_for_checkpoint = reports.reported_mismatches.insert((peer, applied_count));
        (last_common_count, first_report_for_checkpoint)
    };
    if !first_report_for_checkpoint {
        return ChainHashReportStatus::Mismatched;
    }

    let event = DivergenceEvent {
        local_hash,
        remote_hash: hash,
        remote_peer: peer,
        applied_count_at_check: applied_count,
        last_common_count,
    };
    world.trigger(event.clone());
    world.write_message(event);
    ChainHashReportStatus::Mismatched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ComponentId, Mutation, OpaqueId};

    const PEER_A: ReplicationPeerId = ReplicationPeerId([1; 32]);
    const PEER_B: ReplicationPeerId = ReplicationPeerId([2; 32]);
    const COMPONENT_ID: ComponentId = ComponentId(10);

    #[test]
    fn genesis_is_deterministic() {
        let a = ReplicationChain::new();
        let b = ReplicationChain::new();

        assert_eq!(a.current(), b.current());
        assert_eq!(a.count(), 0);
        assert_eq!(a.at_count(0), Some(a.current()));
    }

    #[test]
    fn fold_advances_count_and_history() {
        let mut chain = ReplicationChain::new();

        chain.fold(&tx(PEER_A, 0, 1)).expect("transaction folds");
        assert_eq!(chain.count(), 1);
        assert_eq!(chain.at_count(1), Some(chain.current()));

        chain.fold(&tx(PEER_A, 1, 2)).expect("transaction folds");
        assert_eq!(chain.count(), 2);
        assert_eq!(chain.at_count(2), Some(chain.current()));
    }

    #[test]
    fn same_transaction_sequence_identical_hash() {
        let mut a = ReplicationChain::new();
        let mut b = ReplicationChain::new();
        let sequence = [tx(PEER_A, 0, 1), tx(PEER_B, 0, 2), tx(PEER_A, 1, 3)];

        for transaction in &sequence {
            a.fold(transaction).expect("transaction folds");
            b.fold(transaction).expect("transaction folds");
        }

        assert_eq!(a.current(), b.current());
        assert_eq!(a.count(), sequence.len() as u64);
    }

    #[test]
    fn transaction_order_changes_hash() {
        let mut a = ReplicationChain::new();
        let mut b = ReplicationChain::new();

        for transaction in [tx(PEER_A, 0, 1), tx(PEER_B, 0, 2)] {
            a.fold(&transaction).expect("transaction folds");
        }
        for transaction in [tx(PEER_B, 0, 2), tx(PEER_A, 0, 1)] {
            b.fold(&transaction).expect("transaction folds");
        }

        assert_ne!(a.current(), b.current());
    }

    #[test]
    fn payload_changes_hash() {
        let mut a = ReplicationChain::new();
        let mut b = ReplicationChain::new();

        a.fold(&tx(PEER_A, 0, 1)).expect("transaction folds");
        b.fold(&tx(PEER_A, 0, 2)).expect("transaction folds");

        assert_ne!(a.current(), b.current());
    }

    #[test]
    fn multiple_mutations_fold_as_one_chain_entry() {
        let mut chain = ReplicationChain::new();
        let transaction = Transaction::new(
            PEER_A,
            0,
            vec![mutation(OpaqueId(1), 1), mutation(OpaqueId(2), 2)],
        );

        chain.fold(&transaction).expect("transaction folds");

        assert_eq!(chain.count(), 1);
        assert_eq!(chain.at_count(1), Some(chain.current()));
    }

    #[test]
    fn chain_history_evicts_oldest() {
        let mut chain = ReplicationChain::with_history_capacity(ChainHistoryCapacity(4));

        for sequence in 0..6 {
            chain
                .fold(&tx(PEER_A, sequence, sequence as u8))
                .expect("transaction folds");
        }

        assert_eq!(chain.oldest_available_count(), 2);
        assert_eq!(chain.at_count(0), None);
        assert_eq!(chain.at_count(1), None);
        for count in 2..=6 {
            assert!(chain.at_count(count).is_some(), "count {count} retained");
        }
    }

    fn tx(peer_id: ReplicationPeerId, sequence: u64, value: u8) -> Transaction {
        Transaction::new(peer_id, sequence, vec![mutation(OpaqueId(1), value)])
    }

    fn mutation(entity: OpaqueId, value: u8) -> Mutation {
        Mutation::modify(entity, COMPONENT_ID, vec![value])
    }
}
