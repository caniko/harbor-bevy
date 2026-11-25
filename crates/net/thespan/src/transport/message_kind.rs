//! Transport-level message size budgets.
//!
//! The libp2p transport can only enforce one GossipSub wire cap per builder.
//! Application layers still classify the decoded envelope and apply the
//! per-kind limit before deserializing the inner payload.

/// Ordinary interactive application events.
pub const GAMEPLAY_MESSAGE_MAX_BYTES: usize = 16 * 1024;

/// State-sync and rejoin snapshots. This is also the transport-level cap because
/// it is the largest supported message class.
pub const STATE_SYNC_MESSAGE_MAX_BYTES: usize = 256 * 1024;

/// Default GossipSub max-transmit size.
pub const GOSSIPSUB_MAX_TRANSMIT_SIZE: usize = STATE_SYNC_MESSAGE_MAX_BYTES;

/// Raw request-response/direct-session payload cap.
pub const DIRECT_SESSION_MAX_BYTES: usize = STATE_SYNC_MESSAGE_MAX_BYTES;

/// Stable labels used in logs and telemetry counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMessageKind {
    Gameplay,
    StateSync,
    DirectSession,
    GossipsubEnvelope,
}

impl TransportMessageKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Gameplay => "gameplay",
            Self::StateSync => "state_sync",
            Self::DirectSession => "direct_session",
            Self::GossipsubEnvelope => "gossipsub_envelope",
        }
    }

    pub const fn max_bytes(self) -> usize {
        match self {
            Self::Gameplay => GAMEPLAY_MESSAGE_MAX_BYTES,
            Self::StateSync => STATE_SYNC_MESSAGE_MAX_BYTES,
            Self::DirectSession => DIRECT_SESSION_MAX_BYTES,
            Self::GossipsubEnvelope => GOSSIPSUB_MAX_TRANSMIT_SIZE,
        }
    }
}
