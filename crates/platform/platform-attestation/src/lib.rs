//! Bevy-independent platform attestation core.
//!
//! Typed Steam Web API ticket envelopes and pure verification of
//! already-fetched platform responses: Steam AuthenticateUserTicket JSON and
//! Google-decoded Play Integrity verdicts. This crate performs no HTTP, owns
//! no SDK, and holds no secrets.
//!
//! Reject classes: [`VerifyError::Replayed`], [`VerifyError::Expired`],
//! [`VerifyError::Malformed`], [`VerifyError::WrongApp`],
//! [`VerifyError::WrongPlatform`], and [`VerifyError::Untrusted`].
//!
//! Age signals:
//! - Play: the client reports a Play Age Signals [`PlayAgeClaim`] and binds it
//!   into the Standard Integrity `requestHash` ([`play_request_hash`]) with a
//!   broker-issued nonce. [`verify_play`] accepts the claim only when Google's
//!   decoded verdict proves the genuine, licensed app on a certified device
//!   made that exact request; see [`PlayAgeClaim::protection`] for the mapping.
//! - Steam: no server-verifiable age signal exists, so Steam verification
//!   yields [`ProtectionClass::Unknown`] unless the caller supplies a
//!   [`VerifiedAgeSignal`] (tests only; the broker passes `None`).
//!
//! Clock skew: timestamp comparisons tolerate [`CLOCK_SKEW_SECS`].

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::net::{IpAddr, Ipv6Addr};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Application-specific Play Integrity policy. The domain is hashed verbatim;
/// the client and verifier must configure exactly the same UTF-8 bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestationPolicy {
    pub play_package: String,
    pub age_hash_domain: String,
}

impl AttestationPolicy {
    pub fn new(play_package: impl Into<String>, age_hash_domain: impl Into<String>) -> Self {
        Self {
            play_package: play_package.into(),
            age_hash_domain: age_hash_domain.into(),
        }
    }
}

/// Clock-skew tolerance for timestamp comparisons.
pub const CLOCK_SKEW_SECS: u64 = 60;

/// Lifetime granted to a verified Steam assertion (an AuthenticateUserTicket
/// response carries no expiry of its own).
pub const STEAM_ASSERTION_TTL_SECS: u64 = 300;

/// Lifetime granted to a verified Play assertion.
pub const PLAY_ASSERTION_TTL_SECS: u64 = 300;

/// Oldest accepted Standard Integrity verdict (`requestDetails.timestampMillis`).
/// The token is requested right before submission; the Age Signals consent
/// dialog runs before it and is bounded by [`PLAY_NONCE_TTL_SECS`] instead.
pub const PLAY_VERDICT_MAX_AGE_SECS: u64 = 120;

/// Lifetime of a broker-issued Play nonce. Covers the Age Signals consent
/// dialog plus the Integrity request.
pub const PLAY_NONCE_TTL_SECS: u64 = 300;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Steam,
    Play,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtectionClass {
    VerifiedAdult,
    ProtectedMinor,
    Unknown,
}

/// Raw Steam session ticket bytes. Never logged: [`fmt::Debug`] redacts them.
pub struct SteamTicket {
    pub bytes: Vec<u8>,
    pub identity_hint: Option<String>,
}

impl fmt::Debug for SteamTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SteamTicket")
            .field("bytes", &"<redacted>")
            .field("identity_hint", &self.identity_hint)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AppBinding {
    pub steam_app_id: Option<u32>,
}

impl AppBinding {
    pub const fn new(steam_app_id: Option<u32>) -> Self {
        Self { steam_app_id }
    }
}

pub trait InstantSource {
    fn now_secs(&self) -> u64;
}

pub struct SystemClock;

impl InstantSource for SystemClock {
    fn now_secs(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// Verified result. Holds derived identity only — raw ticket/JWS bytes and raw
/// age evidence are dropped after verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedAssertion {
    pub platform: Platform,
    pub app_id: String,
    pub subject: String,
    pub expires_at: u64,
    pub protection: ProtectionClass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifyError {
    Malformed,
    Expired,
    WrongApp,
    WrongPlatform,
    Replayed,
    /// Device integrity or licensing verdict failed.
    Untrusted,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Malformed => "assertion is malformed or unsuccessful",
            Self::Expired => "assertion is expired",
            Self::WrongApp => "assertion is for a different app",
            Self::WrongPlatform => "assertion is for a different platform",
            Self::Replayed => "assertion was already used",
            Self::Untrusted => "device or license verdict failed",
        })
    }
}

impl Error for VerifyError {}

/// Server-verified age/supervision signal for [`verify_steam`]. Steam has no
/// server-verifiable age source, so the broker passes `None`; tests use it to
/// exercise scope minting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifiedAgeSignal {
    Adult,
    SupervisedMinor,
}

pub trait NonceCache {
    /// Returns `false` when `key` was already seen inside its TTL (replay).
    fn check_and_insert(&mut self, key: &[u8], now_secs: u64, ttl_secs: u64) -> bool;
}

/// In-memory replay cache.
///
// ponytail: O(n) expiry purge per insert; broker restarts bound growth. LRU if long-lived.
#[derive(Debug, Default)]
pub struct MemoryNonceCache {
    entries: HashMap<Vec<u8>, u64>,
}

impl NonceCache for MemoryNonceCache {
    fn check_and_insert(&mut self, key: &[u8], now_secs: u64, ttl_secs: u64) -> bool {
        self.entries.retain(|_, exp| *exp > now_secs);
        if self.entries.contains_key(key) {
            return false;
        }
        self.entries
            .insert(key.to_vec(), now_secs.saturating_add(ttl_secs));
        true
    }
}

fn protection_class(age: Option<VerifiedAgeSignal>) -> ProtectionClass {
    match age {
        Some(VerifiedAgeSignal::Adult) => ProtectionClass::VerifiedAdult,
        Some(VerifiedAgeSignal::SupervisedMinor) => ProtectionClass::ProtectedMinor,
        None => ProtectionClass::Unknown,
    }
}

fn lookup<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut node = value;
    for key in path {
        node = node.get(key)?;
    }
    Some(node)
}

fn first<'a>(value: &'a Value, paths: &[&[&str]]) -> Option<&'a Value> {
    paths.iter().filter_map(|path| lookup(value, path)).next()
}

fn as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Verify an AuthenticateUserTicket-style JSON response for `ticket`.
pub fn verify_steam(
    binding: &AppBinding,
    response_json: &str,
    ticket: &SteamTicket,
    age: Option<VerifiedAgeSignal>,
    cache: &mut impl NonceCache,
    clock: &impl InstantSource,
) -> Result<VerifiedAssertion, VerifyError> {
    const RESULT_PATHS: &[&[&str]] = &[
        &["response", "params", "result"],
        &["response", "result"],
        &["result"],
    ];
    const APPID_PATHS: &[&[&str]] = &[
        &["response", "params", "appid"],
        &["response", "appid"],
        &["appid"],
    ];
    const STEAMID_PATHS: &[&[&str]] = &[
        &["response", "params", "steamid"],
        &["response", "steamid"],
        &["steamid"],
    ];

    let value: Value = serde_json::from_str(response_json).map_err(|_| VerifyError::Malformed)?;
    let result = first(&value, RESULT_PATHS)
        .and_then(as_string)
        .ok_or(VerifyError::Malformed)?;
    if result != "OK" {
        return Err(if result.to_lowercase().contains("expir") {
            VerifyError::Expired
        } else {
            VerifyError::Malformed
        });
    }
    let expected = binding.steam_app_id.ok_or(VerifyError::WrongApp)?;
    let appid = first(&value, APPID_PATHS)
        .and_then(as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .ok_or(VerifyError::Malformed)?;
    if appid != expected {
        return Err(VerifyError::WrongApp);
    }
    let subject = first(&value, STEAMID_PATHS)
        .and_then(as_string)
        .ok_or(VerifyError::Malformed)?;

    let now = clock.now_secs();
    let key = blake3::hash(&ticket.bytes);
    if !cache.check_and_insert(key.as_bytes(), now, STEAM_ASSERTION_TTL_SECS) {
        return Err(VerifyError::Replayed);
    }
    Ok(VerifiedAssertion {
        platform: Platform::Steam,
        app_id: format!("steam:{expected}"),
        subject,
        expires_at: now.saturating_add(STEAM_ASSERTION_TTL_SECS),
        protection: protection_class(age),
    })
}

/// Play Age Signals sharing status reported by the client.
/// `Unavailable` covers Age Signals failures and unspecified statuses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgeSignalsStatus {
    Shared,
    NotShared,
    VerificationRequired,
    Unavailable,
}

/// Play `AgeRangeSource`: `TierA` user-declared, `TierB` parent-managed,
/// `TierC` card/email/selfie/ID check, `TierD` government ID or digital ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgeRangeSource {
    TierA,
    TierB,
    TierC,
    TierD,
}

/// Play `SignificantChangeStatus` for supervised accounts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignificantChange {
    Approved,
    Pending,
    Declined,
}

impl AgeSignalsStatus {
    const NAMES: &[(Self, &str)] = &[
        (Self::Shared, "SHARED"),
        (Self::NotShared, "NOT_SHARED"),
        (Self::VerificationRequired, "VERIFICATION_REQUIRED"),
        (Self::Unavailable, "UNAVAILABLE"),
    ];
}

impl AgeRangeSource {
    const NAMES: &[(Self, &str)] = &[
        (Self::TierA, "TIER_A"),
        (Self::TierB, "TIER_B"),
        (Self::TierC, "TIER_C"),
        (Self::TierD, "TIER_D"),
    ];

    /// Default adult sources: ID-checked tiers only.
    pub const ID_CHECKED: &[Self] = &[Self::TierC, Self::TierD];

    /// Parse a `TIER_X` name (broker `--play-adult-age-sources`).
    pub fn parse(name: &str) -> Option<Self> {
        from_name(Self::NAMES, name)
    }
}

impl SignificantChange {
    const NAMES: &[(Self, &str)] = &[
        (Self::Approved, "APPROVED"),
        (Self::Pending, "PENDING"),
        (Self::Declined, "DECLINED"),
    ];
}

fn from_name<T: Copy>(names: &[(T, &str)], name: &str) -> Option<T> {
    names.iter().find(|(_, n)| *n == name).map(|(v, _)| *v)
}

fn to_name<T: Copy + PartialEq>(names: &[(T, &'static str)], value: T) -> &'static str {
    names
        .iter()
        .find(|(v, _)| *v == value)
        .map_or("", |(_, n)| n)
}

/// Client-reported Play Age Signals result. Never retained: it is parsed,
/// mapped to a [`ProtectionClass`], and dropped. No install id, no dates.
///
/// Canonical form (what the client hashes and submits), empty for null:
/// `status=SHARED;lower=13;upper=15;source=TIER_B;change=APPROVED`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlayAgeClaim {
    pub status: AgeSignalsStatus,
    pub age_lower: Option<u8>,
    pub age_upper: Option<u8>,
    pub source: Option<AgeRangeSource>,
    pub significant_change: Option<SignificantChange>,
}

impl PlayAgeClaim {
    pub fn canonical(&self) -> String {
        let num = |n: Option<u8>| n.map(|n| n.to_string()).unwrap_or_default();
        format!(
            "status={};lower={};upper={};source={};change={}",
            to_name(AgeSignalsStatus::NAMES, self.status),
            num(self.age_lower),
            num(self.age_upper),
            self.source
                .map_or("", |s| to_name(AgeRangeSource::NAMES, s)),
            self.significant_change
                .map_or("", |c| to_name(SignificantChange::NAMES, c)),
        )
    }

    /// Strict parse: only the exact canonical encoding is accepted, so one
    /// claim has exactly one hash.
    pub fn parse(canonical: &str) -> Option<Self> {
        let mut fields = canonical.split(';');
        let mut field = |key: &str| {
            fields
                .next()
                .and_then(|kv| kv.strip_prefix(key))
                .and_then(|kv| kv.strip_prefix('='))
                .map(str::to_string)
        };
        let status = from_name(AgeSignalsStatus::NAMES, &field("status")?)?;
        let (lower, upper) = (field("lower")?, field("upper")?);
        let (source, change) = (field("source")?, field("change")?);
        let age = |v: &str| match v {
            "" => Some(None),
            v => v.parse::<u8>().ok().filter(|n| *n <= 18).map(Some),
        };
        let claim = Self {
            status,
            age_lower: age(&lower)?,
            age_upper: age(&upper)?,
            source: match source.as_str() {
                "" => None,
                s => Some(from_name(AgeRangeSource::NAMES, s)?),
            },
            significant_change: match change.as_str() {
                "" => None,
                c => Some(from_name(SignificantChange::NAMES, c)?),
            },
        };
        if let (Some(lower), Some(upper)) = (claim.age_lower, claim.age_upper)
            && lower > upper
        {
            return None;
        }
        // Round-trip equality also rejects extra fields and padded numbers.
        (claim.canonical() == canonical).then_some(claim)
    }

    /// Map a verified claim:
    /// - `ProtectedMinor`: shared and `ageUpper < 18`, or parent-managed
    ///   (`TierB`) with `ageLower < 18`;
    /// - `VerifiedAdult`: shared, `ageLower >= 18`, no upper bound, and the
    ///   source is one of `adult_sources` (default [`AgeRangeSource::ID_CHECKED`]);
    /// - `Unknown` otherwise (not shared, failures, self-declared adults).
    pub fn protection(&self, adult_sources: &[AgeRangeSource]) -> ProtectionClass {
        if self.status != AgeSignalsStatus::Shared {
            return ProtectionClass::Unknown;
        }
        let minor_upper = self.age_upper.is_some_and(|upper| upper < 18);
        let supervised_minor = self.source == Some(AgeRangeSource::TierB)
            && self.age_lower.is_some_and(|lower| lower < 18);
        if minor_upper || supervised_minor {
            return ProtectionClass::ProtectedMinor;
        }
        let adult = self.age_lower.is_some_and(|lower| lower >= 18)
            && self.age_upper.is_none()
            && self.source.is_some_and(|s| adult_sources.contains(&s));
        if adult {
            ProtectionClass::VerifiedAdult
        } else {
            ProtectionClass::Unknown
        }
    }
}

/// Standard Integrity `requestHash` binding a broker nonce to a canonical
/// claim: `base64url(SHA-256(DOMAIN "\n" nonce "\n" claim))`, unpadded.
pub fn play_request_hash(policy: &AttestationPolicy, nonce: &str, canonical_claim: &str) -> String {
    let digest = Sha256::new()
        .chain_update(policy.age_hash_domain.as_bytes())
        .chain_update(b"\n")
        .chain_update(nonce)
        .chain_update(b"\n")
        .chain_update(canonical_claim)
        .finalize();
    URL_SAFE_NO_PAD.encode(digest)
}

/// Why [`IssuedNonces::insert`] refused a nonce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NonceRefusal {
    /// The global outstanding cap is reached.
    Capacity,
    /// This caller holds too many unspent nonces or asked too often.
    CallerLimited,
}

/// Rate-limit bucket: an IPv4 address, or the /64 an IPv6 caller can rotate
/// addresses inside at no cost.
fn caller_bucket(caller: IpAddr) -> IpAddr {
    match caller {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !u128::from(u64::MAX))),
        },
        v4 => v4,
    }
}

#[derive(Debug, Default)]
struct CallerWindow {
    started: u64,
    issued: usize,
    outstanding: usize,
}

/// Broker-issued single-use Play nonces.
///
/// The issue endpoint is unauthenticated, so issuance is bounded per caller
/// (outstanding nonces and a fixed-window rate) as well as globally: one
/// flooding address cannot starve callers elsewhere.
///
// ponytail: in-memory and capped; a broker restart invalidates outstanding
// nonces (clients just ask again). Shared store if the broker ever scales out.
#[derive(Debug, Default)]
pub struct IssuedNonces {
    issued: HashMap<String, (u64, IpAddr)>,
    callers: HashMap<IpAddr, CallerWindow>,
}

impl IssuedNonces {
    /// Global outstanding-nonce cap.
    pub const MAX_OUTSTANDING: usize = 10_000;

    /// Unspent nonces one caller may hold. Generous enough for a carrier-grade
    /// NAT shared by many players; one assertion needs one nonce.
    pub const MAX_OUTSTANDING_PER_CALLER: usize = 16;

    /// Issuance-rate window for one caller.
    pub const CALLER_WINDOW_SECS: u64 = 60;

    /// Nonces one caller may be issued per [`Self::CALLER_WINDOW_SECS`].
    pub const MAX_ISSUED_PER_CALLER_WINDOW: usize = 30;

    /// Record a freshly generated nonce for `caller`.
    pub fn insert(
        &mut self,
        nonce: String,
        caller: IpAddr,
        now_secs: u64,
    ) -> Result<(), NonceRefusal> {
        self.purge(now_secs);
        let bucket = caller_bucket(caller);
        let window_open =
            |w: &CallerWindow| w.started.saturating_add(Self::CALLER_WINDOW_SECS) > now_secs;
        if let Some(window) = self.callers.get(&bucket) {
            let recent = if window_open(window) {
                window.issued
            } else {
                0
            };
            if window.outstanding >= Self::MAX_OUTSTANDING_PER_CALLER
                || recent >= Self::MAX_ISSUED_PER_CALLER_WINDOW
            {
                return Err(NonceRefusal::CallerLimited);
            }
        }
        if self.issued.len() >= Self::MAX_OUTSTANDING {
            return Err(NonceRefusal::Capacity);
        }
        let window = self.callers.entry(bucket).or_default();
        if !window_open(window) {
            window.started = now_secs;
            window.issued = 0;
        }
        window.issued += 1;
        window.outstanding += 1;
        self.issued.insert(
            nonce,
            (now_secs.saturating_add(PLAY_NONCE_TTL_SECS), bucket),
        );
        Ok(())
    }

    /// Drop expired nonces and idle caller windows.
    fn purge(&mut self, now_secs: u64) {
        let callers = &mut self.callers;
        self.issued.retain(|_, (exp, bucket)| {
            let live = *exp > now_secs;
            if !live && let Some(window) = callers.get_mut(bucket) {
                window.outstanding = window.outstanding.saturating_sub(1);
            }
            live
        });
        self.callers.retain(|_, window| {
            window.outstanding > 0
                || window.started.saturating_add(Self::CALLER_WINDOW_SECS) > now_secs
        });
    }

    /// Spend a nonce. Unknown or already-spent nonces are
    /// [`VerifyError::Replayed`]; issued-but-stale ones are
    /// [`VerifyError::Expired`].
    pub fn consume(&mut self, nonce: &str, now_secs: u64) -> Result<(), VerifyError> {
        let (exp, bucket) = self.issued.remove(nonce).ok_or(VerifyError::Replayed)?;
        if let Some(window) = self.callers.get_mut(&bucket) {
            window.outstanding = window.outstanding.saturating_sub(1);
        }
        if exp <= now_secs {
            return Err(VerifyError::Expired);
        }
        Ok(())
    }
}

/// Verify a Google-decoded Standard Integrity verdict (the
/// `decodeIntegrityToken` response JSON) for `claim` bound to `nonce`.
/// The caller must already have spent `nonce` via [`IssuedNonces::consume`].
///
/// Requires: `requestPackageName` is the release package, `requestHash`
/// matches [`play_request_hash`], the verdict is fresh, the app is
/// `PLAY_RECOGNIZED`, the device `MEETS_DEVICE_INTEGRITY`, and the paid app is
/// `LICENSED`. The verdict and claim are dropped; only the class survives.
pub fn verify_play(
    policy: &AttestationPolicy,
    decoded_json: &str,
    nonce: &str,
    claim: &str,
    adult_sources: &[AgeRangeSource],
    clock: &impl InstantSource,
) -> Result<VerifiedAssertion, VerifyError> {
    let value: Value = serde_json::from_str(decoded_json).map_err(|_| VerifyError::Malformed)?;
    let payload = value
        .get("tokenPayloadExternal")
        .ok_or(VerifyError::Malformed)?;
    let text = |path: &[&str]| lookup(payload, path).and_then(as_string);

    let package = text(&["requestDetails", "requestPackageName"]).ok_or(VerifyError::Malformed)?;
    if package != policy.play_package {
        return Err(VerifyError::WrongApp);
    }
    let request_hash = text(&["requestDetails", "requestHash"]).ok_or(VerifyError::Malformed)?;
    if request_hash != play_request_hash(policy, nonce, claim) {
        return Err(VerifyError::Malformed);
    }
    let timestamp_secs = lookup(payload, &["requestDetails", "timestampMillis"])
        .and_then(as_u64)
        .ok_or(VerifyError::Malformed)?
        / 1000;
    let now = clock.now_secs();
    if timestamp_secs.saturating_add(PLAY_VERDICT_MAX_AGE_SECS) < now {
        return Err(VerifyError::Expired);
    }
    if timestamp_secs > now.saturating_add(CLOCK_SKEW_SECS) {
        return Err(VerifyError::Malformed);
    }
    if text(&["appIntegrity", "appRecognitionVerdict"]).as_deref() != Some("PLAY_RECOGNIZED") {
        return Err(VerifyError::WrongApp);
    }
    if text(&["appIntegrity", "packageName"]).is_some_and(|p| p != policy.play_package) {
        return Err(VerifyError::WrongApp);
    }
    let device_ok = lookup(payload, &["deviceIntegrity", "deviceRecognitionVerdict"])
        .and_then(Value::as_array)
        .is_some_and(|labels| {
            labels
                .iter()
                .any(|label| label.as_str() == Some("MEETS_DEVICE_INTEGRITY"))
        });
    let licensed = text(&["accountDetails", "appLicensingVerdict"]).as_deref() == Some("LICENSED");
    if !device_ok || !licensed {
        return Err(VerifyError::Untrusted);
    }
    let claim = PlayAgeClaim::parse(claim).ok_or(VerifyError::Malformed)?;
    Ok(VerifiedAssertion {
        platform: Platform::Play,
        app_id: format!("play:{package}"),
        // Play verdicts carry no account identity; the package is the subject.
        subject: package,
        expires_at: now.saturating_add(PLAY_ASSERTION_TTL_SECS),
        protection: claim.protection(adult_sources),
    })
}
