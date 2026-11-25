//! Wire-level errors. Distinct from in-process Rust error types so they can
//! cross language/version boundaries unchanged.

#[derive(Debug, Clone, Copy, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[repr(u16)]
pub enum ErrorCode {
    /// The frame did not decode, or violated invariants (size, charset).
    Malformed = 1,
    /// Signature did not verify, or signer mismatched the named peer.
    AuthFailed = 2,
    /// `issued_at` outside the server's accepted skew window.
    Stale = 3,
    /// Replay: nonce already seen for this signer.
    Replayed = 4,
    /// Unknown namespace, or no live registration to act on.
    NotFound = 5,
    /// Caller exceeded a rate limit.
    RateLimited = 6,
    /// Server is shutting down or otherwise unavailable.
    Unavailable = 7,
    /// Catch-all for unexpected server-side failure.
    Internal = 8,
}

#[derive(Debug, Clone, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ProtoError {
    pub code: ErrorCode,
    /// Human-readable detail. Not stable; logs only.
    pub detail: String,
}

impl ProtoError {
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}
