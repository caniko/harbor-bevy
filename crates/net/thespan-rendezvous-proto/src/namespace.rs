//! Discovery namespaces.
//!
//! A namespace is the bucket peers register and search in. The server treats
//! it opaquely — these constants are convention only, but keeping them
//! centralized prevents typos that would silently partition the lobby graph.

/// Newtype wrapper around a namespace string.
///
/// Bounded length is enforced by [`Namespace::new`]. The server rejects
/// unbounded strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct Namespace(String);

impl Namespace {
    pub const MAX_LEN: usize = 128;

    pub fn new(s: impl Into<String>) -> Result<Self, NamespaceError> {
        let s = s.into();
        if s.is_empty() {
            return Err(NamespaceError::Empty);
        }
        if s.len() > Self::MAX_LEN {
            return Err(NamespaceError::TooLong);
        }
        if !s
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_'))
        {
            return Err(NamespaceError::InvalidChar);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NamespaceError {
    #[error("namespace must not be empty")]
    Empty,
    #[error("namespace exceeds {} bytes", Namespace::MAX_LEN)]
    TooLong,
    #[error("namespace must match [a-z0-9._-]+")]
    InvalidChar,
}

/// Conventional namespace constants. Use these instead of stringly typing
/// at call sites.
pub mod well_known {
    pub const PUBLIC_LOBBY: &str = "lobby.public";
    pub const QUICKMATCH_1V1: &str = "quickmatch.1v1";
    pub const QUICKMATCH_TOURNAMENT: &str = "quickmatch.tournament";

    /// Per-tournament namespace; format the tournament id in.
    pub fn tournament(id: &str) -> String {
        format!("lobby.tournament.{id}")
    }

    /// Per-friend-code namespace for direct invites.
    pub fn invite(code: &str) -> String {
        format!("invite.{code}")
    }
}
