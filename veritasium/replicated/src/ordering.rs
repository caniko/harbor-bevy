use std::cmp::Ordering;

use crate::{NetworkId, ReplicationPeerId, Transaction};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransactionKey {
    pub peer_id: ReplicationPeerId,
    pub sequence: u64,
}

impl TransactionKey {
    pub fn new(peer_id: ReplicationPeerId, sequence: u64) -> Self {
        Self { peer_id, sequence }
    }

    pub fn from_transaction<Id: NetworkId>(transaction: &Transaction<Id>) -> Self {
        Self {
            peer_id: transaction.peer_id,
            sequence: transaction.sequence,
        }
    }
}

impl Ord for TransactionKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.sequence
            .cmp(&other.sequence)
            .then_with(|| self.peer_id.cmp(&other.peer_id))
    }
}

impl PartialOrd for TransactionKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalOrderingError {
    DuplicateTransactionKey(TransactionKey),
}

impl core::fmt::Display for CanonicalOrderingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DuplicateTransactionKey(key) => write!(
                f,
                "duplicate transaction key {:?}:{}",
                key.peer_id, key.sequence
            ),
        }
    }
}

impl std::error::Error for CanonicalOrderingError {}

pub fn canonical_transaction_cmp<Id: NetworkId>(
    left: &Transaction<Id>,
    right: &Transaction<Id>,
) -> Ordering {
    TransactionKey::from_transaction(left).cmp(&TransactionKey::from_transaction(right))
}

pub fn canonicalize_transactions<Id: NetworkId>(
    mut transactions: Vec<Transaction<Id>>,
) -> Result<Vec<Transaction<Id>>, CanonicalOrderingError> {
    transactions.sort_by(canonical_transaction_cmp);
    for pair in transactions.windows(2) {
        let left = TransactionKey::from_transaction(&pair[0]);
        let right = TransactionKey::from_transaction(&pair[1]);
        if left == right {
            return Err(CanonicalOrderingError::DuplicateTransactionKey(left));
        }
    }
    Ok(transactions)
}
