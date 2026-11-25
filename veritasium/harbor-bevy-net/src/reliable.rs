//! Bounded, per-stream reliability. Acknowledgements and replay do not apply events.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    marker::PhantomData,
    ops::RangeInclusive,
    time::{Duration, Instant},
};

use bevy::prelude::*;
use bevy_replicated::{dedup::SeenSet, epoch::SessionEpoch};
use rustc_hash::FxHashMap;
use veritasium_wire::{OutboundSeq, PlayerId};

use crate::{ConnectionLifecyclePlugin, GameNet};

const RECENT_HASHES: usize = 4096;
const MAX_UNACKED: usize = 512;
const MAX_SACK_RANGES: usize = 16;
const MAX_ACK_WINDOW: u64 = 4096;
const MAX_REORDERED: usize = 256;
const DEGRADED_RETRIES: u8 = 5;
const MAX_RETRIES: u8 = 10;
const MAX_PAYLOAD_BYTES: usize = crate::MAX_SESSION_PACKET_BYTES - 256;

/// Declaration order is replay priority (highest first).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[repr(u8)]
pub enum ProtocolStream {
    TurnBoundary,
    BattleAction,
    Formation,
    Commitment,
    StateCheck,
    AckNack,
    Session,
    Telemetry,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct MessageId {
    pub sender: PlayerId,
    pub stream: ProtocolStream,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct MatchEnvelope {
    pub protocol_version: u16,
    pub run_id: [u8; 16],
    pub match_id: [u8; 16],
    pub session_epoch: u64,
    pub connection_epoch: u64,
    pub sender_player: PlayerId,
    pub stream: ProtocolStream,
    pub seq: u64,
    pub epoch: SessionEpoch,
    pub kind_id: u16,
    pub payload_hash: [u8; 32],
    pub message_id: MessageId,
    pub payload: Vec<u8>,
}

/// Expected values for an admitted sender, per match. Reset receive state for
/// a new session, not when a transport merely reconnects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchContext {
    pub protocol_version: u16,
    pub run_id: [u8; 16],
    pub match_id: [u8; 16],
    pub session_epoch: u64,
    pub connection_epoch: u64,
    pub sender: PlayerId,
}

impl MatchEnvelope {
    pub fn validate(&self, expected: MatchContext) -> Result<(), SessionFault> {
        if self.protocol_version != expected.protocol_version {
            return Err(SessionFault::ProtocolVersionMismatch);
        }
        if self.run_id != expected.run_id
            || self.match_id != expected.match_id
            || self.session_epoch != expected.session_epoch
            || self.connection_epoch != expected.connection_epoch
            || self.sender_player != expected.sender
        {
            return Err(violation(
                "envelope is outside the admitted sender's session",
            ));
        }
        self.validate_contents()
    }

    fn validate_contents(&self) -> Result<(), SessionFault> {
        if self.seq == 0
            || self.sender_player.index() > 1
            || self.epoch.match_id != self.match_id
            || self.message_id
                != (MessageId {
                    sender: self.sender_player,
                    stream: self.stream,
                    seq: self.seq,
                })
        {
            return Err(violation("inconsistent envelope identity"));
        }
        if self.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(violation("envelope payload exceeds the session limit"));
        }
        if self.payload_hash != *blake3::hash(&self.payload).as_bytes() {
            return Err(SessionFault::PayloadDigestMismatch);
        }
        Ok(())
    }

    fn fingerprint(&self) -> ([u8; 32], SessionEpoch, u16) {
        (self.payload_hash, self.epoch, self.kind_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Event, thiserror::Error)]
pub enum SessionFault {
    #[error("Session validation failed: {0}")]
    ValidationViolation(String),
    #[error("Envelope payload digest does not match")]
    PayloadDigestMismatch,
    #[error("Message identity reused with different contents")]
    ConflictingDuplicate,
    #[error("Session protocol versions do not match")]
    ProtocolVersionMismatch,
    #[error("Reliable session queue overflowed")]
    OverflowDisconnect,
    #[error("Replay retries exhausted for {0:?}")]
    ReplayExhausted(MessageId),
    #[error("Peer state diverged")]
    Desync,
    #[error("Peer reveal does not match the commitment")]
    CommitmentMismatch,
    #[error("Commitment retries exhausted")]
    CommitmentRetriesExhausted,
}

fn violation(reason: &str) -> SessionFault {
    SessionFault::ValidationViolation(reason.into())
}

/// Deduplicate at delivery, after any required reordering. Check old packets
/// against this cache as well so recent conflicting replays are not lost.
#[derive(Debug, Default)]
pub struct DedupeCache {
    seen: SeenSet<(PlayerId, ProtocolStream), u64>,
    recent: FxHashMap<MessageId, ([u8; 32], SessionEpoch, u16)>,
    insertion_order: VecDeque<MessageId>,
}

impl DedupeCache {
    pub fn try_accept(&mut self, envelope: &MatchEnvelope) -> Result<bool, SessionFault> {
        envelope.validate_contents()?;
        let id = envelope.message_id;
        let fingerprint = envelope.fingerprint();
        if let Some(previous) = self.recent.get(&id) {
            return if *previous == fingerprint {
                Ok(false)
            } else {
                Err(SessionFault::ConflictingDuplicate)
            };
        }
        if !self.seen.try_accept((id.sender, id.stream), id.seq) {
            return Ok(false);
        }
        if self.recent.len() == RECENT_HASHES
            && let Some(oldest) = self.insertion_order.pop_front()
        {
            self.recent.remove(&oldest);
        }
        self.recent.insert(id, fingerprint);
        self.insertion_order.push_back(id);
        Ok(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    pub contiguous: u64,
    pub selective: Vec<RangeInclusive<u64>>,
}

impl Ack {
    fn validate(&self, sent: u64) -> Result<(), SessionFault> {
        let limit = sent.min(self.contiguous.saturating_add(MAX_ACK_WINDOW));
        let mut last = self.contiguous;
        if self.contiguous > sent || self.selective.len() > MAX_SACK_RANGES {
            return Err(violation("acknowledgement exceeds sent window"));
        }
        for range in &self.selective {
            if range.is_empty() || *range.start() <= last || *range.end() > limit {
                return Err(violation("invalid selective acknowledgement range"));
            }
            last = *range.end();
        }
        Ok(())
    }

    fn includes(&self, seq: u64) -> bool {
        seq <= self.contiguous || self.selective.iter().any(|range| range.contains(&seq))
    }
}

/// One tracker per sender and stream.
#[derive(Debug, Default)]
pub struct AckTracker {
    contiguous: u64,
    gaps: BTreeSet<u64>,
}

impl AckTracker {
    pub fn observe(&mut self, seq: u64) -> Result<bool, SessionFault> {
        if seq == 0 || seq > self.contiguous.saturating_add(MAX_ACK_WINDOW) {
            return Err(violation("sequence is outside the acknowledgement window"));
        }
        if seq <= self.contiguous || !self.gaps.insert(seq) {
            return Ok(false);
        }
        while let Some(next) = self.contiguous.checked_add(1) {
            if !self.gaps.remove(&next) {
                break;
            }
            self.contiguous = next;
        }
        Ok(true)
    }

    pub fn snapshot(&self) -> Ack {
        let mut ranges: Vec<RangeInclusive<u64>> = vec![];
        for &seq in &self.gaps {
            if let Some(last) = ranges.last_mut()
                && last.end().checked_add(1) == Some(seq)
            {
                *last = *last.start()..=seq;
                continue;
            }
            if ranges.len() == MAX_SACK_RANGES {
                break;
            }
            ranges.push(seq..=seq);
        }
        Ack {
            contiguous: self.contiguous,
            selective: ranges,
        }
    }
}

#[derive(Debug)]
pub struct ReorderBuffer {
    sender: PlayerId,
    stream: ProtocolStream,
    delivered: u64,
    pending: BTreeMap<u64, MatchEnvelope>,
}

impl ReorderBuffer {
    pub fn new(sender: PlayerId, stream: ProtocolStream) -> Self {
        Self {
            sender,
            stream,
            delivered: 0,
            pending: BTreeMap::new(),
        }
    }

    pub fn delivered_through(&self) -> u64 {
        self.delivered
    }

    /// Old packets are checked by the delivery-side `DedupeCache`; callers
    /// can test `delivered_through()` before passing new packets here.
    pub fn push(&mut self, envelope: MatchEnvelope) -> Result<Vec<MatchEnvelope>, SessionFault> {
        envelope.validate_contents()?;
        if (envelope.sender_player, envelope.stream) != (self.sender, self.stream) {
            return Err(violation("envelope belongs to a different reorder stream"));
        }
        let seq = envelope.seq;
        if seq <= self.delivered {
            return Ok(Vec::new());
        }
        if let Some(previous) = self.pending.get(&seq) {
            return if previous.fingerprint() == envelope.fingerprint() {
                Ok(Vec::new())
            } else {
                Err(SessionFault::ConflictingDuplicate)
            };
        }
        if seq - self.delivered > MAX_ACK_WINDOW {
            return Err(violation("sequence is outside the reorder window"));
        }
        if seq - self.delivered != 1 {
            if self.pending.len() == MAX_REORDERED {
                return Err(SessionFault::OverflowDisconnect);
            }
            self.pending.insert(seq, envelope);
            return Ok(Vec::new());
        }
        let mut ready = vec![envelope];
        self.delivered = seq;
        while let Some(next) = self.delivered.checked_add(1) {
            let Some(packet) = self.pending.remove(&next) else {
                break;
            };
            self.delivered = next;
            ready.push(packet);
        }
        Ok(ready)
    }
}

#[derive(Debug)]
struct Unacked {
    envelope: MatchEnvelope,
    next_replay: Instant,
    attempts: u8,
}

#[derive(Debug, Default)]
pub struct UnackedBuffer(BTreeMap<(ProtocolStream, u64), Unacked>);

impl UnackedBuffer {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn ensure_room(&self) -> Result<(), SessionFault> {
        if self.len() == MAX_UNACKED {
            Err(SessionFault::OverflowDisconnect)
        } else {
            Ok(())
        }
    }

    fn insert(&mut self, envelope: MatchEnvelope, now: Instant) {
        self.0.insert(
            (envelope.stream, envelope.seq),
            Unacked {
                envelope,
                next_replay: now + replay_delay(0),
                attempts: 0,
            },
        );
    }
}

#[derive(Debug)]
pub struct Replay {
    pub envelope: MatchEnvelope,
    pub degraded: bool,
}

#[derive(Debug, Resource)]
pub struct ReliableSender {
    context: MatchContext,
    seq: [OutboundSeq; ProtocolStream::Telemetry as usize + 1],
    unacked: UnackedBuffer,
}

impl ReliableSender {
    pub fn new(context: MatchContext) -> Self {
        Self {
            context,
            seq: [OutboundSeq(0); ProtocolStream::Telemetry as usize + 1],
            unacked: UnackedBuffer::default(),
        }
    }

    pub fn unacked(&self) -> &UnackedBuffer {
        &self.unacked
    }

    pub fn set_connection_epoch(&mut self, epoch: u64) {
        self.context.connection_epoch = epoch;
    }

    pub fn send(
        &mut self,
        stream: ProtocolStream,
        epoch: SessionEpoch,
        kind_id: u16,
        payload: Vec<u8>,
        now: Instant,
    ) -> Result<MatchEnvelope, SessionFault> {
        self.unacked.ensure_room()?;
        if payload.len() > MAX_PAYLOAD_BYTES
            || self.context.sender.index() > 1
            || epoch.match_id != self.context.match_id
        {
            return Err(violation("invalid outgoing session envelope"));
        }
        let counter = &mut self.seq[stream as usize];
        let seq = counter
            .raw()
            .checked_add(1)
            .ok_or_else(|| violation("outbound sequence exhausted"))?;
        let envelope = MatchEnvelope {
            protocol_version: self.context.protocol_version,
            run_id: self.context.run_id,
            match_id: self.context.match_id,
            session_epoch: self.context.session_epoch,
            connection_epoch: self.context.connection_epoch,
            sender_player: self.context.sender,
            stream,
            seq,
            epoch,
            kind_id,
            payload_hash: *blake3::hash(&payload).as_bytes(),
            message_id: MessageId {
                sender: self.context.sender,
                stream,
                seq,
            },
            payload,
        };
        self.unacked.insert(envelope.clone(), now);
        *counter = OutboundSeq(seq);
        Ok(envelope)
    }

    pub fn acknowledge(&mut self, stream: ProtocolStream, ack: &Ack) -> Result<(), SessionFault> {
        ack.validate(self.seq[stream as usize].raw())?;
        self.unacked
            .0
            .retain(|&(kind, seq), _| kind != stream || !ack.includes(seq));
        Ok(())
    }

    pub fn replay_due(&mut self, now: Instant) -> Result<Vec<Replay>, SessionFault> {
        if let Some(exhausted) = self
            .unacked
            .0
            .values()
            .find(|item| item.next_replay <= now && item.attempts == MAX_RETRIES)
        {
            return Err(SessionFault::ReplayExhausted(exhausted.envelope.message_id));
        }
        let mut pending = vec![];
        for item in self
            .unacked
            .0
            .values_mut()
            .filter(|item| item.next_replay <= now)
        {
            item.attempts += 1;
            item.next_replay = now + replay_delay(item.attempts);
            let mut envelope = item.envelope.clone();
            envelope.connection_epoch = self.context.connection_epoch;
            pending.push(Replay {
                envelope,
                degraded: item.attempts >= DEGRADED_RETRIES,
            });
        }
        Ok(pending)
    }
}

fn replay_delay(attempt: u8) -> Duration {
    Duration::from_secs_f64((0.1 * 1.5_f64.powi(i32::from(attempt))).min(2.0))
}

pub struct ReliablePlugin<G: GameNet>(PhantomData<fn() -> G>);

impl<G: GameNet> Default for ReliablePlugin<G> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<G: GameNet> Plugin for ReliablePlugin<G> {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<ConnectionLifecyclePlugin>() {
            app.add_plugins(ConnectionLifecyclePlugin);
        }
    }
}
