//! In-memory registration store with TTL eviction.
//!
//! Crash-safe by design: state is ephemeral. Clients re-announce every
//! ≤ `MAX_REGISTRATION_TTL_SECS` via Heartbeat; a server restart costs at most
//! one heartbeat cycle of staleness for the network as a whole.

use parking_lot::RwLock;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use thespan_rendezvous_proto::ticket::{PeerIdBytes, PeerTicket};

#[derive(Debug, Clone)]
pub struct Registration {
    pub registration_id: u64,
    pub namespace: String,
    pub ticket: PeerTicket,
    pub metadata: Vec<u8>,
    pub expires_at: Instant,
}

pub struct Store {
    inner: RwLock<Inner>,
    capacity: usize,
}

struct Inner {
    next_id: u64,
    /// Primary index: `(namespace, peer_id)` → registration.
    by_key: HashMap<(String, Vec<u8>), Registration>,
    /// Replay-protection nonce ledger: `(peer_id, nonce)` → `seen_at`.
    /// Pruned by `prune_expired`.
    nonces: HashMap<(Vec<u8>, [u8; 16]), Instant>,
    /// FIFO of nonce keys for cheap expiry-driven eviction.
    nonce_order: VecDeque<NonceEntry>,
}

type NonceKey = (Vec<u8>, [u8; 16]);
type NonceEntry = (NonceKey, Instant);

impl Store {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(Inner {
                next_id: 1,
                by_key: HashMap::new(),
                nonces: HashMap::new(),
                nonce_order: VecDeque::new(),
            }),
            capacity,
        }
    }

    /// Insert or replace a registration. Returns the granted `registration_id`.
    /// Returns `None` if the store is at capacity and the entry is new.
    pub fn upsert(
        &self,
        namespace: String,
        peer_id: PeerIdBytes,
        ticket: PeerTicket,
        metadata: Vec<u8>,
        ttl: Duration,
    ) -> Option<u64> {
        let mut g = self.inner.write();
        let key = (namespace.clone(), peer_id.0.clone());
        let now = Instant::now();
        if let Some(existing) = g.by_key.get_mut(&key) {
            existing.ticket = ticket;
            existing.metadata = metadata;
            existing.expires_at = now + ttl;
            return Some(existing.registration_id);
        }
        if g.by_key.len() >= self.capacity {
            return None;
        }
        let id = g.next_id;
        g.next_id += 1;
        let reg = Registration {
            registration_id: id,
            namespace: namespace.clone(),
            ticket,
            metadata,
            expires_at: now + ttl,
        };
        g.by_key.insert(key, reg);
        Some(id)
    }

    pub fn heartbeat(&self, namespace: &str, peer_id: &PeerIdBytes, ttl: Duration) -> Option<u64> {
        let mut g = self.inner.write();
        let key = (namespace.to_owned(), peer_id.0.clone());
        let reg = g.by_key.get_mut(&key)?;
        reg.expires_at = Instant::now() + ttl;
        Some(reg.registration_id)
    }

    pub fn unregister(&self, namespace: &str, peer_id: &PeerIdBytes) {
        let mut g = self.inner.write();
        let key = (namespace.to_owned(), peer_id.0.clone());
        g.by_key.remove(&key);
    }

    /// Page through namespace entries. `cursor` here is a byte-encoded u64 offset
    /// into a sorted snapshot — simple, robust, no need for stable pagination.
    pub fn discover(
        &self,
        namespace: &str,
        cursor: &[u8],
        limit: usize,
    ) -> (Vec<Registration>, Vec<u8>) {
        let offset = if cursor.len() == 8 {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(cursor);
            u64::from_be_bytes(buf) as usize
        } else {
            0
        };
        let now = Instant::now();
        let g = self.inner.read();
        let mut all: Vec<&Registration> = g
            .by_key
            .values()
            .filter(|r| r.namespace == namespace && r.expires_at > now)
            .collect();
        all.sort_by_key(|r| r.registration_id);
        let end = (offset + limit).min(all.len());
        let page: Vec<Registration> = all[offset..end].iter().map(|r| (*r).clone()).collect();
        let next_cursor = if end < all.len() {
            (end as u64).to_be_bytes().to_vec()
        } else {
            Vec::new()
        };
        (page, next_cursor)
    }

    /// Returns true if the nonce is fresh (was inserted), false on replay.
    pub fn check_and_record_nonce(
        &self,
        peer_id: &PeerIdBytes,
        nonce: [u8; 16],
        replay_window: Duration,
    ) -> bool {
        let mut g = self.inner.write();
        let now = Instant::now();
        // Cheap incremental prune of expired nonces from the front of the FIFO.
        while let Some((_, t)) = g.nonce_order.front()
            && now.duration_since(*t) > replay_window
        {
            let (k, _) = g.nonce_order.pop_front().unwrap();
            g.nonces.remove(&k);
        }
        let key = (peer_id.0.clone(), nonce);
        if g.nonces.contains_key(&key) {
            return false;
        }
        g.nonces.insert(key.clone(), now);
        g.nonce_order.push_back((key, now));
        true
    }

    /// Drop expired registrations. Cheap; called from a background tick.
    pub fn prune_expired(&self) -> usize {
        let mut g = self.inner.write();
        let now = Instant::now();
        let before = g.by_key.len();
        g.by_key
            .retain(|_, registration| registration.expires_at > now);
        before - g.by_key.len()
    }

    pub fn len(&self) -> usize {
        self.inner.read().by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().by_key.is_empty()
    }
}
