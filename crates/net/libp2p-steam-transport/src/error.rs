use std::fmt;

/// Errors that can occur in the Steam relay transport layer.
#[derive(Debug)]
pub enum SteamTransportError {
    /// Failed to create a Steam P2P listen socket.
    ListenFailed,
    /// Failed to establish a connection to a remote Steam peer.
    ConnectFailed(String),
    /// The dial operation was cancelled (receiver dropped).
    DialCancelled,
    /// A generic error from the Steam SDK.
    Steam(String),
}

impl fmt::Display for SteamTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ListenFailed => write!(f, "failed to create listen socket"),
            Self::ConnectFailed(msg) => write!(f, "failed to connect: {msg}"),
            Self::DialCancelled => write!(f, "dial cancelled"),
            Self::Steam(msg) => write!(f, "steam error: {msg}"),
        }
    }
}

impl std::error::Error for SteamTransportError {}
