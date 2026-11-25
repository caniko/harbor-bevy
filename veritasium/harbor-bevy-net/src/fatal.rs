use harbor_bevy_core::fatal::FatalCode;

#[derive(Debug, thiserror::Error)]
pub enum NetFatal {
    #[error("Peer state diverged")]
    Desync,
    #[error("Connection lost: {reason}")]
    ConnectionLost { reason: String },
    #[error("Commitment failed after {retries} retries")]
    CommitmentFailure { retries: u32 },
    #[error("Peer not ready after {attempts} attempts in {elapsed_secs:.2}s")]
    PeerReadyTimeout { attempts: u32, elapsed_secs: f32 },
}

impl FatalCode for NetFatal {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Desync => 12,
            Self::ConnectionLost { .. } => 13,
            Self::CommitmentFailure { .. } => 15,
            Self::PeerReadyTimeout { .. } => 18,
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Desync => "desync",
            Self::ConnectionLost { .. } => "connection_lost",
            Self::CommitmentFailure { .. } => "commitment_failure",
            Self::PeerReadyTimeout { .. } => "peer_ready_timeout",
        }
    }
}
