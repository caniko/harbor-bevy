//! Bevy-independent release metadata: version identity, public HTTPS URL gate,
//! and artifact-manifest schema. No hashing, tagging, or dist writes.

use std::error::Error;
use std::fmt;
use std::net::IpAddr;

use url::{Host, Url};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseChannel {
    EarlyAccess,
    Stable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleaseVersion<'a> {
    pub semver: &'a str,
    pub channel: ReleaseChannel,
}

impl<'a> ReleaseVersion<'a> {
    pub const fn visible_text(self) -> &'a str {
        self.semver
    }

    pub const fn android_version_name(self) -> &'a str {
        self.semver
    }

    pub fn android_version_code(
        self,
        policy: AndroidVersionCodePolicy,
    ) -> Result<u32, VersionError> {
        let (major, minor, patch) = parse_semver(self.semver)?;
        policy.encode(major, minor, patch)
    }
}

/// Android `versionCode` = `major * 1_000_000 + minor * 10_000 + patch`.
///
/// Component bounds (collision-free and monotonic): major `0..=4293`,
/// minor `0..=99`, patch `0..=9999`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AndroidVersionCodePolicy;

impl AndroidVersionCodePolicy {
    pub const MAJOR_WEIGHT: u32 = 1_000_000;
    pub const MINOR_WEIGHT: u32 = 10_000;
    pub const MAX_MAJOR: u32 = (u32::MAX - (99 * 10_000 + 9_999)) / 1_000_000;
    pub const MAX_MINOR: u32 = 99;
    pub const MAX_PATCH: u32 = 9_999;

    pub fn encode(self, major: u32, minor: u32, patch: u32) -> Result<u32, VersionError> {
        if major > Self::MAX_MAJOR || minor > Self::MAX_MINOR || patch > Self::MAX_PATCH {
            return Err(VersionError::OutOfRange);
        }
        major
            .checked_mul(Self::MAJOR_WEIGHT)
            .and_then(|n| n.checked_add(minor.checked_mul(Self::MINOR_WEIGHT)?))
            .and_then(|n| n.checked_add(patch))
            .ok_or(VersionError::OutOfRange)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionError {
    InvalidSemver,
    OutOfRange,
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidSemver => {
                "semver must be major.minor.patch with decimal components and no leading zeroes"
            }
            Self::OutOfRange => {
                "version-code component exceeds major 0..=4293, minor 0..=99, or patch 0..=9999"
            }
        })
    }
}

impl Error for VersionError {}

pub fn is_release_application_id(candidate: &str, expected: &str) -> bool {
    !expected.is_empty() && candidate == expected
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicUrls {
    pub base: String,
    pub privacy: String,
    pub eula: String,
    pub support: String,
    pub broker: String,
    pub diagnostics: String,
    /// Store "data deletion" URL: the deletion section of the diagnostics page.
    pub deletion: String,
}

impl PublicUrls {
    pub fn from_base(url: &str) -> Result<Self, PublicUrlError> {
        let parsed = parse_public_https(url)?;
        let mut dir = parsed.clone();
        if !dir.path().ends_with('/') {
            dir.set_path(&format!("{}/", dir.path()));
        }
        let page = |name: &str| {
            dir.join(name)
                .map(|joined| joined.to_string())
                .map_err(|_| PublicUrlError::Malformed)
        };
        Ok(Self {
            privacy: page("privacy")?,
            eula: page("eula")?,
            support: page("support")?,
            broker: page("broker")?,
            diagnostics: page("diagnostics")?,
            deletion: page("diagnostics#delete-data")?,
            base: parsed.as_str().trim_end_matches('/').to_string(),
        })
    }

    pub fn from_optional(url: Option<&str>) -> Result<Self, PublicUrlError> {
        match url.map(str::trim).filter(|s| !s.is_empty()) {
            None => Err(PublicUrlError::MissingPublicUrl),
            Some(url) => Self::from_base(url),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicUrlError {
    MissingPublicUrl,
    NotHttps,
    EmptyHost,
    Placeholder,
    Credentials,
    QueryOrFragment,
    Malformed,
}

impl fmt::Display for PublicUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingPublicUrl => "public HTTPS base URL is missing",
            Self::NotHttps => "public URL must use https",
            Self::EmptyHost => "public URL host is empty",
            Self::Placeholder => "public URL host is a placeholder",
            Self::Credentials => "public URL must not include credentials",
            Self::QueryOrFragment => "public URL must not include query or fragment",
            Self::Malformed => "public URL is malformed",
        })
    }
}

impl Error for PublicUrlError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactMeta {
    pub checksum_sha256: String,
    pub source_sha: String,
    pub profile: String,
    pub platform: String,
    pub signed: bool,
}

impl ArtifactMeta {
    pub fn manifest_complete(&self, required_signed: bool) -> Result<(), IncompleteManifest> {
        let empty = |s: &str| s.trim().is_empty();
        if empty(&self.checksum_sha256)
            || empty(&self.source_sha)
            || empty(&self.profile)
            || empty(&self.platform)
            || (required_signed && !self.signed)
        {
            return Err(IncompleteManifest);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IncompleteManifest;

impl fmt::Display for IncompleteManifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("artifact manifest is incomplete or unsigned")
    }
}

impl Error for IncompleteManifest {}

fn parse_semver(semver: &str) -> Result<(u32, u32, u32), VersionError> {
    let mut parts = semver.split('.');
    let major = parse_component(parts.next().ok_or(VersionError::InvalidSemver)?)?;
    let minor = parse_component(parts.next().ok_or(VersionError::InvalidSemver)?)?;
    let patch = parse_component(parts.next().ok_or(VersionError::InvalidSemver)?)?;
    if parts.next().is_some() {
        return Err(VersionError::InvalidSemver);
    }
    Ok((major, minor, patch))
}

fn parse_component(part: &str) -> Result<u32, VersionError> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(VersionError::InvalidSemver);
    }
    if part.len() > 1 && part.starts_with('0') {
        return Err(VersionError::InvalidSemver);
    }
    part.parse().map_err(|_| VersionError::InvalidSemver)
}

fn parse_public_https(url: &str) -> Result<Url, PublicUrlError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(PublicUrlError::MissingPublicUrl);
    }
    let parsed = Url::parse(url).map_err(|_| PublicUrlError::Malformed)?;
    if parsed.cannot_be_a_base() {
        return Err(PublicUrlError::Malformed);
    }
    if parsed.scheme() != "https" {
        return Err(PublicUrlError::NotHttps);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(PublicUrlError::Credentials);
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(PublicUrlError::QueryOrFragment);
    }
    let Some(host) = parsed.host() else {
        return Err(PublicUrlError::EmptyHost);
    };
    if is_blocked_host(host) {
        return Err(PublicUrlError::Placeholder);
    }
    Ok(parsed)
}

fn is_blocked_host(host: Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => is_placeholder_host(domain),
        Host::Ipv4(ip) => blocked_ip(IpAddr::V4(ip)),
        Host::Ipv6(ip) => {
            blocked_ip(IpAddr::V6(ip))
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| blocked_ip(IpAddr::V4(v4)))
        }
    }
}

fn blocked_ip(ip: IpAddr) -> bool {
    ip.is_loopback() || ip.is_unspecified()
}

fn is_placeholder_host(host: &str) -> bool {
    host.trim_end_matches('.')
        .to_ascii_lowercase()
        .split('.')
        .any(|label| matches!(label, "localhost" | "example"))
}
