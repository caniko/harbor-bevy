//! Fixed little-endian version headers over opaque payloads.

use thiserror::Error;

/// A frame did not match the expected header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum VersionedFrameError {
    #[error("frame magic does not match")]
    InvalidMagic,
    #[error("frame lacks a complete version prefix")]
    TruncatedVersion,
    #[error("unsupported frame version {0}")]
    UnsupportedVersion(u16),
}

/// Split a two-byte LE version and return the body only for an allowed version.
pub fn split_versioned_frame<'a>(
    bytes: &'a [u8],
    supported_versions: &[u16],
) -> Result<(u16, &'a [u8]), VersionedFrameError> {
    let (version, body) = bytes
        .split_first_chunk::<2>()
        .ok_or(VersionedFrameError::TruncatedVersion)?;
    let version = u16::from_le_bytes(*version);
    if !supported_versions.contains(&version) {
        return Err(VersionedFrameError::UnsupportedVersion(version));
    }
    Ok((version, body))
}

/// Check the caller's magic bytes before splitting the version and body.
pub fn split_magic_versioned_frame<'a>(
    bytes: &'a [u8],
    magic: &[u8],
    supported_versions: &[u16],
) -> Result<(u16, &'a [u8]), VersionedFrameError> {
    let bytes = bytes
        .strip_prefix(magic)
        .ok_or(VersionedFrameError::InvalidMagic)?;
    split_versioned_frame(bytes, supported_versions)
}

/// Write a two-byte LE version followed by an opaque body.
pub fn build_versioned_frame(version: u16, body: &[u8]) -> Vec<u8> {
    build_magic_versioned_frame(&[], version, body)
}

/// Write magic bytes, a two-byte LE version, then an opaque body.
pub fn build_magic_versioned_frame(magic: &[u8], version: u16, body: &[u8]) -> Vec<u8> {
    [magic, &version.to_le_bytes(), body].concat()
}
