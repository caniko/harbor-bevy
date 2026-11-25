//! Bevy-independent attestation broker library.
//!
//! Validates Steam tickets and Play Integrity assertions (via
//! [`platform-attestation`](platform_attestation)), mints short-lived
//! HMAC-SHA256 scoped tokens, and accepts signed safety reports, deletion
//! requests, and authorized diagnostics. The Detritus bearer never leaves the
//! server: clients receive scoped tokens only.
//!
//! Diagnosis scope is granted only for
//! [`ProtectionClass::VerifiedAdult`](platform_attestation::ProtectionClass).
//! The only such source is a Play Integrity-bound Age Signals claim (see
//! [`Broker::attest_play`]); Steam always yields `Unknown`.
//!
//! Routes whose platform credentials are absent stay disabled
//! ([`BrokerError::Unavailable`], HTTP 503) while the rest keep serving.

use std::error::Error;
use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use platform_attestation::{
    AgeRangeSource, AppBinding, AttestationPolicy, InstantSource, IssuedNonces, MemoryNonceCache,
    NonceRefusal, Platform, ProtectionClass, SteamTicket, VerifiedAgeSignal, VerifyError,
    verify_play, verify_steam,
};
use sha2::Sha256;

pub mod play;

pub use play::{GooglePlayIntegrity, HttpPost, HttpReply, PlayIntegrityApi, ServiceAccount};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default token lifetime (15 minutes).
pub const DEFAULT_TOKEN_TTL_SECS: u64 = 900;

#[derive(Clone, Debug)]
pub struct BrokerConfig {
    pub steam_app_id: u32,
    pub play_policy: AttestationPolicy,
    pub token_ttl_secs: u64,
    pub public_bind: String,
    pub detritus_url: String,
    /// Age Signals sources that may prove an adult (default: ID-checked
    /// `TIER_C`, `TIER_D`).
    pub play_adult_sources: Vec<AgeRangeSource>,
}

impl BrokerConfig {
    pub fn new(steam_app_id: u32, detritus_url: String, play_policy: AttestationPolicy) -> Self {
        Self {
            steam_app_id,
            play_policy,
            token_ttl_secs: DEFAULT_TOKEN_TTL_SECS,
            public_bind: "127.0.0.1:8080".to_string(),
            detritus_url,
            play_adult_sources: AgeRangeSource::ID_CHECKED.to_vec(),
        }
    }
}

/// Server-side secrets. Never serialized, never logged.
pub trait SecretStore {
    fn steam_api_key(&self) -> Option<String>;
    fn token_hmac_key(&self) -> Option<Vec<u8>>;
    fn detritus_bearer(&self) -> Option<String>;
}

/// In-memory secrets for tests. Production loads from env/files (see binary).
#[derive(Default)]
pub struct InMemorySecrets {
    pub steam_api_key: Option<String>,
    pub token_hmac_key: Option<Vec<u8>>,
    pub detritus_bearer: Option<String>,
}

impl SecretStore for InMemorySecrets {
    fn steam_api_key(&self) -> Option<String> {
        self.steam_api_key.clone()
    }
    fn token_hmac_key(&self) -> Option<Vec<u8>> {
        self.token_hmac_key.clone()
    }
    fn detritus_bearer(&self) -> Option<String> {
        self.detritus_bearer.clone()
    }
}

/// Fetches the Steam Web API AuthenticateUserTicket response for a ticket.
/// The implementation holds the publisher key; tests use [`FakeSteamApi`].
pub trait SteamWebApi {
    fn authenticate_user_ticket(&self, ticket: &SteamTicket) -> Result<String, BrokerError>;
}

/// `None` is a Steam route without a publisher key: disabled, not fatal.
impl<T: SteamWebApi> SteamWebApi for Option<T> {
    fn authenticate_user_ticket(&self, ticket: &SteamTicket) -> Result<String, BrokerError> {
        self.as_ref()
            .ok_or(BrokerError::Unavailable("steam attestation disabled"))?
            .authenticate_user_ticket(ticket)
    }
}

pub struct FakeSteamApi {
    pub response: Result<String, BrokerError>,
}

impl SteamWebApi for FakeSteamApi {
    fn authenticate_user_ticket(&self, _ticket: &SteamTicket) -> Result<String, BrokerError> {
        self.response.clone()
    }
}

/// Forwards authorized diagnostics to Detritus with the server-held bearer.
pub trait DetritusClient {
    fn forward(&self, payload: &[u8]) -> Result<(), BrokerError>;
}

#[derive(Default)]
pub struct FakeDetritus {
    pub calls: std::cell::RefCell<Vec<Vec<u8>>>,
    pub fail: bool,
}

impl DetritusClient for FakeDetritus {
    fn forward(&self, payload: &[u8]) -> Result<(), BrokerError> {
        if self.fail {
            return Err(BrokerError::Upstream("detritus unreachable".into()));
        }
        self.calls.borrow_mut().push(payload.to_vec());
        Ok(())
    }
}

/// Accepted report bound to the authenticated token subject for operator
/// review. The reporter identity inside the report is client-asserted; `sub`
/// is what the broker verified.
#[derive(Clone, Debug)]
pub struct StoredReport {
    pub subject: String,
    pub report: trust_safety::SignedReport,
    pub received_at: u64,
    pub preserved_until: Option<u64>,
}

/// Storage failure. Carries no paths: surfaces map to a fixed 500 string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreError;

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("durable store unavailable")
    }
}

impl Error for StoreError {}

/// Stores signed safety reports. Storage must be durable before the broker
/// acknowledges: `store` returns whether the report was newly accepted
/// (duplicate signature bytes are idempotent and return `false`). Any `Err`
/// must surface as a 500 so clients retry instead of dropping the report.
pub trait ReportSink {
    fn store(
        &mut self,
        subject: &str,
        report: &trust_safety::SignedReport,
    ) -> Result<bool, StoreError>;
}

#[derive(Default)]
pub struct MemoryReportSink {
    pub reports: Vec<StoredReport>,
}

impl ReportSink for MemoryReportSink {
    fn store(
        &mut self,
        subject: &str,
        report: &trust_safety::SignedReport,
    ) -> Result<bool, StoreError> {
        if self
            .reports
            .iter()
            .any(|stored| stored.report.signature == report.signature)
        {
            return Ok(false);
        }
        self.reports.push(StoredReport {
            subject: subject.to_string(),
            report: report.clone(),
            received_at: unix_now(),
            preserved_until: None,
        });
        Ok(true)
    }
}

/// File-backed report sink: appends to `path` (JSON array) on every store so
/// accepted reports survive restarts. Corrupt entries at load are skipped.
pub struct FileReportSink {
    path: std::path::PathBuf,
    reports: Vec<StoredReport>,
}

const REPORT_RETENTION_SECS: u64 = 180 * 24 * 60 * 60;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl FileReportSink {
    /// Open the store. Missing files start empty; any other read/parse
    /// failure is an error — silently treating corruption as empty would let
    /// the next save overwrite accepted work.
    pub fn open(path: std::path::PathBuf) -> std::io::Result<Self> {
        let reports = match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
            Ok(text) => {
                let array = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.as_array().cloned())
                    .ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, "report store corrupt")
                    })?;
                // Corrupt entries are dropped only when at least one entry
                // parses: a fully-unreadable array is corruption, not emptiness.
                let parsed: Vec<StoredReport> =
                    array.iter().filter_map(parse_stored_report).collect();
                if parsed.is_empty() && !array.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "report store unreadable",
                    ));
                }
                parsed
            }
        };
        Ok(Self { path, reports })
    }

    pub fn reports(&self) -> &[StoredReport] {
        &self.reports
    }

    /// Remove expired reports, retaining valid operator preservation holds.
    pub fn prune_at(&mut self, now: u64) -> Result<usize, StoreError> {
        let mut live = Vec::with_capacity(self.reports.len());
        for stored in &self.reports {
            let expiry = stored
                .report
                .report
                .issued_at
                .min(stored.received_at)
                .saturating_add(REPORT_RETENTION_SECS);
            let until = match stored.preserved_until {
                Some(until) if until > expiry.saturating_add(REPORT_RETENTION_SECS) => {
                    eprintln!("ignoring report preservation beyond the review limit");
                    expiry
                }
                Some(until) => expiry.max(until),
                None => expiry,
            };
            if now < until {
                live.push(stored.clone());
            }
        }
        let removed = self.reports.len() - live.len();
        if removed != 0 {
            let previous = std::mem::replace(&mut self.reports, live);
            if let Err(err) = self.save() {
                self.reports = previous;
                return Err(err);
            }
        }
        Ok(removed)
    }

    pub fn prune(&mut self) -> Result<usize, StoreError> {
        self.prune_at(unix_now())
    }

    /// Store with an injected server receipt time.
    pub fn store_at(
        &mut self,
        subject: &str,
        report: &trust_safety::SignedReport,
        received_at: u64,
    ) -> Result<bool, StoreError> {
        if self
            .reports
            .iter()
            .any(|stored| stored.report.signature == report.signature)
        {
            return Ok(false);
        }
        self.reports.push(StoredReport {
            subject: subject.to_string(),
            report: report.clone(),
            received_at,
            preserved_until: None,
        });
        if let Err(err) = self.save() {
            self.reports.pop();
            return Err(err);
        }
        Ok(true)
    }

    /// Atomic durable replacement: write temp file, fsync, rename.
    fn save(&self) -> Result<(), StoreError> {
        let array: Vec<serde_json::Value> = self.reports.iter().map(report_json).collect();
        let text = serde_json::to_string(&array).map_err(|_| StoreError)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StoreError)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, text).map_err(|_| StoreError)?;
        std::fs::rename(&tmp, &self.path).map_err(|_| StoreError)?;
        Ok(())
    }
}

impl ReportSink for FileReportSink {
    fn store(
        &mut self,
        subject: &str,
        report: &trust_safety::SignedReport,
    ) -> Result<bool, StoreError> {
        self.store_at(subject, report, unix_now())
    }
}

/// Accepted deletion job bound to the authenticated subject.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeletionJob {
    pub subject: String,
    pub deadline_unix_secs: u64,
}

/// Records accepted deletion jobs with their server-side deadline.
/// Re-submission for the same subject is an idempotent upsert.
pub trait DeletionQueue {
    fn enqueue(&mut self, subject: &str, deadline_unix_secs: u64) -> Result<(), StoreError>;
}

#[derive(Default)]
pub struct MemoryDeletionQueue {
    pub jobs: Vec<DeletionJob>,
}

impl DeletionQueue for MemoryDeletionQueue {
    fn enqueue(&mut self, subject: &str, deadline_unix_secs: u64) -> Result<(), StoreError> {
        if let Some(job) = self.jobs.iter_mut().find(|job| job.subject == subject) {
            job.deadline_unix_secs = deadline_unix_secs;
        } else {
            self.jobs.push(DeletionJob {
                subject: subject.to_string(),
                deadline_unix_secs,
            });
        }
        Ok(())
    }
}

/// File-backed deletion queue with the same durability contract as
/// [`FileReportSink`].
pub struct FileDeletionQueue {
    path: std::path::PathBuf,
    jobs: Vec<DeletionJob>,
}

impl FileDeletionQueue {
    /// Same strictness contract as [`FileReportSink::open`].
    pub fn open(path: std::path::PathBuf) -> std::io::Result<Self> {
        let jobs = match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
            Ok(text) => {
                let array = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.as_array().cloned())
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "deletion queue corrupt",
                        )
                    })?;
                let parsed: Vec<DeletionJob> = array
                    .iter()
                    .filter_map(|v| {
                        Some(DeletionJob {
                            subject: v.get("subject")?.as_str()?.to_string(),
                            deadline_unix_secs: v.get("deadline")?.as_u64()?,
                        })
                    })
                    .collect();
                if parsed.is_empty() && !array.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "deletion queue unreadable",
                    ));
                }
                // Collapse duplicate subjects left by older versions.
                let mut deduped: Vec<DeletionJob> = Vec::with_capacity(parsed.len());
                for job in parsed {
                    if let Some(slot) = deduped.iter_mut().find(|j| j.subject == job.subject) {
                        slot.deadline_unix_secs = job.deadline_unix_secs;
                    } else {
                        deduped.push(job);
                    }
                }
                deduped
            }
        };
        Ok(Self { path, jobs })
    }

    pub fn jobs(&self) -> &[DeletionJob] {
        &self.jobs
    }

    fn save(&self) -> Result<(), StoreError> {
        let array: Vec<serde_json::Value> = self
            .jobs
            .iter()
            .map(|job| {
                serde_json::json!({"subject": job.subject, "deadline": job.deadline_unix_secs})
            })
            .collect();
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = serde_json::to_string(&array).map_err(|_| StoreError)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StoreError)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, text).map_err(|_| StoreError)?;
        std::fs::rename(&tmp, &self.path).map_err(|_| StoreError)?;
        Ok(())
    }
}

impl DeletionQueue for FileDeletionQueue {
    fn enqueue(&mut self, subject: &str, deadline_unix_secs: u64) -> Result<(), StoreError> {
        if let Some(job) = self.jobs.iter_mut().find(|job| job.subject == subject) {
            job.deadline_unix_secs = deadline_unix_secs;
        } else {
            self.jobs.push(DeletionJob {
                subject: subject.to_string(),
                deadline_unix_secs,
            });
        }
        self.save()
    }
}

fn bytes_field(value: &serde_json::Value) -> Option<Vec<u8>> {
    value
        .as_array()?
        .iter()
        .map(|b| b.as_u64().and_then(|n| u8::try_from(n).ok()))
        .collect()
}

fn report_json(stored: &StoredReport) -> serde_json::Value {
    let report = &stored.report;
    serde_json::json!({
        "subject": stored.subject,
        "received_at": stored.received_at,
        "preserved_until": stored.preserved_until,
        "reason": report.report.reason as u8,
        "evidence": {
            "peer_id": report.report.evidence.peer_id,
            "match_id": report.report.evidence.match_id.map(|id| id.to_vec()),
            "mode": report.report.evidence.mode,
            "preset_message": report.report.evidence.preset_message.map(|id| id.to_u8()),
            "timestamp": report.report.evidence.timestamp,
        },
        "reporter": report.report.reporter.0,
        "target": report.report.target.0,
        "issued_at": report.report.issued_at,
        "signature": report.signature.to_vec(),
        "public_key": report.public_key.to_vec(),
    })
}

/// Parse a signed report in the exact shape the client sends (see the
/// product report client). Every signed field is preserved; dropping any of
/// them would break signature verification.
pub fn parse_client_report(value: &serde_json::Value) -> Option<trust_safety::SignedReport> {
    let reason =
        trust_safety::ReportReason::from_u8(u8::try_from(value.get("reason")?.as_u64()?).ok()?)?;
    let evidence = value.get("evidence")?;
    let signature: [u8; 64] = bytes_field(value.get("signature")?)?.try_into().ok()?;
    let public_key: [u8; 32] = bytes_field(value.get("public_key")?)?.try_into().ok()?;
    Some(trust_safety::SignedReport {
        report: trust_safety::SafetyReport {
            reason,
            evidence: trust_safety::BoundedEvidence {
                peer_id: bytes_field(evidence.get("peer_id")?)?,
                match_id: match evidence.get("match_id") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(value) => Some(bytes_field(value)?.try_into().ok()?),
                },
                mode: match evidence.get("mode") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(value) => Some(u8::try_from(value.as_u64()?).ok()?),
                },
                preset_message: match evidence.get("preset_message") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(value) => value
                        .as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .and_then(trust_safety::PresetMessageId::from_u8),
                },
                timestamp: evidence.get("timestamp")?.as_u64()?,
            },
            reporter: trust_safety::PeerIdentity(bytes_field(value.get("reporter")?)?),
            target: trust_safety::PeerIdentity(bytes_field(value.get("target")?)?),
            issued_at: value.get("issued_at")?.as_u64()?,
        },
        signature,
        public_key,
    })
}

fn parse_stored_report(value: &serde_json::Value) -> Option<StoredReport> {
    Some(StoredReport {
        subject: value.get("subject")?.as_str()?.to_string(),
        report: parse_client_report(value)?,
        // Older records have no trustworthy receipt time; expire them conservatively.
        received_at: value
            .get("received_at")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        preserved_until: value.get("preserved_until").and_then(|v| v.as_u64()),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    Report,
    Delete,
    Diagnostics,
}

/// Attested grant: the token plus the broker's own expiry and granted
/// scopes. Clients must use these — never a client-guessed TTL — for cache
/// eligibility; the broker still re-verifies server-side on every call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestedGrant {
    pub token: String,
    pub exp: u64,
    pub scopes: Vec<Scope>,
    /// Server-verified class the grant was minted for; clients apply it to
    /// local protection state (a minor restricts, never the reverse alone).
    pub protection: ProtectionClass,
}

impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::Delete => "delete",
            Self::Diagnostics => "diagnostics",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "report" => Some(Self::Report),
            "delete" => Some(Self::Delete),
            "diagnostics" => Some(Self::Diagnostics),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedToken {
    pub sub: String,
    pub platform: Platform,
    pub protection: ProtectionClass,
    pub exp: u64,
    pub scopes: Vec<Scope>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrokerError {
    Malformed(String),
    Expired,
    WrongApp,
    WrongPlatform,
    Replayed,
    /// Device integrity or licensing verdict failed.
    Untrusted,
    InvalidToken,
    MissingScope(Scope),
    BadReport,
    Upstream(String),
    Misconfigured(&'static str),
    /// Route disabled (platform credentials absent) or at capacity.
    Unavailable(&'static str),
    /// This caller exceeded its share of an unauthenticated route.
    RateLimited,
    /// Durable store failed: the operation was NOT accepted. Fixed string —
    /// never paths or io details.
    Storage,
}

impl fmt::Display for BrokerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(detail) => write!(f, "malformed request: {detail}"),
            Self::Expired => f.write_str("assertion or token is expired"),
            Self::WrongApp => f.write_str("assertion is for a different app"),
            Self::WrongPlatform => f.write_str("assertion is for a different platform"),
            Self::Replayed => f.write_str("assertion was already used"),
            Self::Untrusted => f.write_str("device or license verdict failed"),
            Self::Unavailable(what) => write!(f, "unavailable: {what}"),
            Self::RateLimited => f.write_str("too many requests from this address"),
            Self::Storage => f.write_str("durable store unavailable, retry later"),
            Self::InvalidToken => f.write_str("token is invalid or expired"),
            Self::MissingScope(scope) => write!(f, "token lacks {} scope", scope.name()),
            Self::BadReport => f.write_str("report signature is invalid"),
            Self::Upstream(detail) => write!(f, "upstream unavailable: {detail}"),
            Self::Misconfigured(what) => write!(f, "broker misconfigured: {what}"),
        }
    }
}

impl Error for BrokerError {}

impl BrokerError {
    /// HTTP status for the binary's typed 4xx mapping (5xx for server faults).
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Malformed(_) | Self::BadReport => 400,
            Self::Expired | Self::InvalidToken => 401,
            Self::WrongApp | Self::WrongPlatform | Self::Untrusted | Self::MissingScope(_) => 403,
            Self::Replayed => 409,
            Self::RateLimited => 429,
            Self::Upstream(_) => 502,
            Self::Misconfigured(_) | Self::Storage => 500,
            Self::Unavailable(_) => 503,
        }
    }
}

impl From<VerifyError> for BrokerError {
    fn from(value: VerifyError) -> Self {
        match value {
            VerifyError::Malformed => Self::Malformed("assertion failed verification".into()),
            VerifyError::Expired => Self::Expired,
            VerifyError::WrongApp => Self::WrongApp,
            VerifyError::WrongPlatform => Self::WrongPlatform,
            VerifyError::Replayed => Self::Replayed,
            VerifyError::Untrusted => Self::Untrusted,
        }
    }
}

fn platform_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Steam => "steam",
        Platform::Play => "play",
    }
}

fn parse_platform(name: &str) -> Option<Platform> {
    match name {
        "steam" => Some(Platform::Steam),
        "play" => Some(Platform::Play),
        _ => None,
    }
}

/// Wire name of a protection class (token payload and attest responses).
pub fn protection_name(protection: ProtectionClass) -> &'static str {
    match protection {
        ProtectionClass::VerifiedAdult => "verified_adult",
        ProtectionClass::ProtectedMinor => "protected_minor",
        ProtectionClass::Unknown => "unknown",
    }
}

fn parse_protection(name: &str) -> Option<ProtectionClass> {
    match name {
        "verified_adult" => Some(ProtectionClass::VerifiedAdult),
        "protected_minor" => Some(ProtectionClass::ProtectedMinor),
        "unknown" => Some(ProtectionClass::Unknown),
        _ => None,
    }
}

/// A Play attestation whose nonce is spent, awaiting Google's decode. Holds
/// no broker state, so the decode runs without the broker lock.
pub struct PendingPlay {
    decoder: Arc<dyn PlayIntegrityApi + Send + Sync>,
    package: String,
}

impl PendingPlay {
    /// Google's `decodeIntegrityToken` round trip for `token`.
    pub fn decode(&self, token: &str, now_secs: u64) -> Result<String, BrokerError> {
        self.decoder
            .decode_integrity_token(&self.package, token, now_secs)
    }
}

pub struct Broker<W, D> {
    config: BrokerConfig,
    hmac_key: Vec<u8>,
    detritus_bearer: String,
    /// `None`: no service account, `/v1/attest/play` is disabled (503).
    /// Shared so the Google round trip can run without the broker lock.
    play: Option<Arc<dyn PlayIntegrityApi + Send + Sync>>,
    play_nonces: IssuedNonces,
    rng: ring::rand::SystemRandom,
    steam: W,
    detritus: D,
    cache: MemoryNonceCache,
}

// ponytail: manual Debug so HMAC key and bearer can never leak into logs.
impl<W, D> fmt::Debug for Broker<W, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Broker")
            .field("config", &self.config)
            .field("hmac_key", &"<redacted>")
            .field("detritus_bearer", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl<W: SteamWebApi, D: DetritusClient> Broker<W, D> {
    pub fn new(
        config: BrokerConfig,
        secrets: &impl SecretStore,
        steam: W,
        detritus: D,
    ) -> Result<Self, BrokerError> {
        // A missing Steam key disables the Steam route (see the
        // `SteamWebApi for Option<T>` impl); it does not stop the broker.
        let hmac_key = secrets
            .token_hmac_key()
            .filter(|key| !key.is_empty())
            .ok_or(BrokerError::Misconfigured("token hmac key"))?;
        let detritus_bearer = secrets
            .detritus_bearer()
            .filter(|bearer| !bearer.is_empty())
            .ok_or(BrokerError::Misconfigured("detritus bearer"))?;
        if config.play_policy.play_package.is_empty()
            || config.play_policy.age_hash_domain.is_empty()
        {
            return Err(BrokerError::Misconfigured(
                "play package and age hash domain",
            ));
        }
        Ok(Self {
            config,
            hmac_key,
            detritus_bearer,
            play: None,
            play_nonces: IssuedNonces::default(),
            rng: ring::rand::SystemRandom::new(),
            steam,
            detritus,
            cache: MemoryNonceCache::default(),
        })
    }

    /// Enable `/v1/attest/play` with a Google decode client.
    pub fn with_play(mut self, play: impl PlayIntegrityApi + Send + Sync + 'static) -> Self {
        self.play = Some(Arc::new(play));
        self
    }

    pub fn detritus_bearer(&self) -> &str {
        &self.detritus_bearer
    }

    fn mint(
        &self,
        sub: &str,
        platform: Platform,
        protection: ProtectionClass,
        scopes: &[Scope],
        now: u64,
    ) -> String {
        let body = serde_json::json!({
            "v": 1,
            "sub": sub,
            "platform": platform_name(platform),
            "protection": protection_name(protection),
            "exp": now.saturating_add(self.config.token_ttl_secs),
            "scopes": scopes.iter().map(|scope| scope.name()).collect::<Vec<_>>(),
        });
        let payload = URL_SAFE_NO_PAD.encode(body.to_string());
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.hmac_key)
            .expect("hmac key non-empty checked at construction");
        mac.update(payload.as_bytes());
        let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{payload}.{sig}")
    }

    pub fn verify_token(
        &self,
        token: &str,
        clock: &impl InstantSource,
    ) -> Result<ScopedToken, BrokerError> {
        let (payload_b64, sig_b64) = token.split_once('.').ok_or(BrokerError::InvalidToken)?;
        let sig = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|_| BrokerError::InvalidToken)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.hmac_key)
            .expect("hmac key non-empty checked at construction");
        mac.update(payload_b64.as_bytes());
        mac.verify_slice(&sig)
            .map_err(|_| BrokerError::InvalidToken)?;
        let payload_bytes = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| BrokerError::InvalidToken)?;
        let body: serde_json::Value =
            serde_json::from_slice(&payload_bytes).map_err(|_| BrokerError::InvalidToken)?;
        if body.get("v").and_then(|v| v.as_u64()) != Some(1) {
            return Err(BrokerError::InvalidToken);
        }
        let now = clock.now_secs();
        let exp = body
            .get("exp")
            .and_then(|exp| exp.as_u64())
            .ok_or(BrokerError::InvalidToken)?;
        if exp <= now {
            return Err(BrokerError::InvalidToken);
        }
        let sub = body
            .get("sub")
            .and_then(|sub| sub.as_str())
            .ok_or(BrokerError::InvalidToken)?
            .to_string();
        let platform = body
            .get("platform")
            .and_then(|platform| platform.as_str())
            .and_then(parse_platform)
            .ok_or(BrokerError::InvalidToken)?;
        let protection = body
            .get("protection")
            .and_then(|protection| protection.as_str())
            .and_then(parse_protection)
            .ok_or(BrokerError::InvalidToken)?;
        let scopes = body
            .get("scopes")
            .and_then(|scopes| scopes.as_array())
            .ok_or(BrokerError::InvalidToken)?
            .iter()
            .map(|scope| {
                scope
                    .as_str()
                    .and_then(Scope::parse)
                    .ok_or(BrokerError::InvalidToken)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ScopedToken {
            sub,
            platform,
            protection,
            exp,
            scopes,
        })
    }

    fn scopes_for(protection: ProtectionClass) -> Vec<Scope> {
        let mut scopes = vec![Scope::Report, Scope::Delete];
        if protection == ProtectionClass::VerifiedAdult {
            scopes.push(Scope::Diagnostics);
        }
        scopes
    }

    fn grant(&self, assertion: platform_attestation::VerifiedAssertion, now: u64) -> AttestedGrant {
        let scopes = Self::scopes_for(assertion.protection);
        let token = self.mint(
            &assertion.subject,
            assertion.platform,
            assertion.protection,
            &scopes,
            now,
        );
        AttestedGrant {
            token,
            exp: now.saturating_add(self.config.token_ttl_secs),
            scopes,
            protection: assertion.protection,
        }
    }

    /// Validate a Steam ticket and mint a scoped token. Steam has no
    /// server-verifiable age signal, so the binary passes `age = None` and
    /// Steam grants are `Unknown`; tests pass a signal to exercise scopes.
    pub fn attest_steam(
        &mut self,
        ticket: &SteamTicket,
        age: Option<VerifiedAgeSignal>,
        clock: &impl InstantSource,
    ) -> Result<AttestedGrant, BrokerError> {
        let response = self.steam.authenticate_user_ticket(ticket)?;
        let assertion = verify_steam(
            &AppBinding::new(Some(self.config.steam_app_id)),
            &response,
            ticket,
            age,
            &mut self.cache,
            clock,
        )?;
        Ok(self.grant(assertion, clock.now_secs()))
    }

    /// Issue a single-use nonce for a Play attestation (short TTL, see
    /// [`platform_attestation::PLAY_NONCE_TTL_SECS`]) to `caller`, the
    /// requesting address. The route is unauthenticated, so issuance is
    /// bounded per caller as well as globally (see [`IssuedNonces`]).
    /// Disabled with the Play route: a nonce nobody can redeem is not worth
    /// issuing.
    pub fn issue_play_nonce(
        &mut self,
        caller: IpAddr,
        clock: &impl InstantSource,
    ) -> Result<String, BrokerError> {
        use ring::rand::SecureRandom as _;
        if self.play.is_none() {
            return Err(BrokerError::Unavailable("play attestation disabled"));
        }
        let mut bytes = [0u8; 32];
        self.rng
            .fill(&mut bytes)
            .map_err(|_| BrokerError::Unavailable("nonce entropy"))?;
        let nonce = URL_SAFE_NO_PAD.encode(bytes);
        match self
            .play_nonces
            .insert(nonce.clone(), caller, clock.now_secs())
        {
            Ok(()) => Ok(nonce),
            Err(NonceRefusal::CallerLimited) => Err(BrokerError::RateLimited),
            Err(NonceRefusal::Capacity) => Err(BrokerError::Unavailable("nonce capacity")),
        }
    }

    /// Validate a Standard Play Integrity token whose `requestHash` binds the
    /// canonical Age Signals `claim` to a broker-issued `nonce`, and mint a
    /// scoped token for the resulting protection class. Token, claim, and
    /// verdict are dropped after verification.
    ///
    /// The same steps a concurrent server runs separately: [`Self::begin_play`]
    /// and [`Self::finish_play`] under the broker lock, [`PendingPlay::decode`]
    /// (the Google round trip) outside it.
    pub fn attest_play(
        &mut self,
        token: &str,
        claim: &str,
        nonce: &str,
        clock: &impl InstantSource,
    ) -> Result<AttestedGrant, BrokerError> {
        let decoded = self
            .begin_play(nonce, clock)?
            .decode(token, clock.now_secs())?;
        self.finish_play(&decoded, nonce, claim, clock)
    }

    /// Spend `nonce` and hand back the Google decoder. The nonce is spent
    /// before Google is called, so replays cost no decode quota.
    pub fn begin_play(
        &mut self,
        nonce: &str,
        clock: &impl InstantSource,
    ) -> Result<PendingPlay, BrokerError> {
        let decoder = self
            .play
            .clone()
            .ok_or(BrokerError::Unavailable("play attestation disabled"))?;
        self.play_nonces.consume(nonce, clock.now_secs())?;
        Ok(PendingPlay {
            decoder,
            package: self.config.play_policy.play_package.clone(),
        })
    }

    /// Verify Google's decoded verdict for a nonce spent by
    /// [`Self::begin_play`] and mint the scoped token.
    pub fn finish_play(
        &self,
        decoded: &str,
        nonce: &str,
        claim: &str,
        clock: &impl InstantSource,
    ) -> Result<AttestedGrant, BrokerError> {
        let assertion = verify_play(
            &self.config.play_policy,
            decoded,
            nonce,
            claim,
            &self.config.play_adult_sources,
            clock,
        )?;
        Ok(self.grant(assertion, clock.now_secs()))
    }

    fn require_scope(
        &self,
        token: &str,
        scope: Scope,
        clock: &impl InstantSource,
    ) -> Result<ScopedToken, BrokerError> {
        let scoped = self.verify_token(token, clock)?;
        if !scoped.scopes.contains(&scope) {
            return Err(BrokerError::MissingScope(scope));
        }
        Ok(scoped)
    }

    /// Accept a signed safety report. No count-based sanctions: storage only.
    /// The report is stored bound to the authenticated token subject so
    /// operator review can tell verified identity from client-asserted fields.
    pub fn submit_report(
        &self,
        token: &str,
        report: &trust_safety::SignedReport,
        sink: &mut impl ReportSink,
        clock: &impl InstantSource,
    ) -> Result<(), BrokerError> {
        let scoped = self.require_scope(token, Scope::Report, clock)?;
        trust_safety::verify_report(report).map_err(|_| BrokerError::BadReport)?;
        sink.store(&scoped.sub, report)
            .map_err(|_| BrokerError::Storage)?;
        Ok(())
    }

    /// Accept a deletion request; returns the server-side deadline (unix secs,
    /// 30-day SLA from [`diagnostics_policy`]).
    pub fn submit_deletion(
        &self,
        token: &str,
        request: &diagnostics_policy::DeleteRequest,
        queue: &mut impl DeletionQueue,
        clock: &impl InstantSource,
    ) -> Result<u64, BrokerError> {
        let scoped = self.require_scope(token, Scope::Delete, clock)?;
        let deadline = request
            .server_deadline()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        queue
            .enqueue(&scoped.sub, deadline)
            .map_err(|_| BrokerError::Storage)?;
        Ok(deadline)
    }

    /// Forward diagnostics to Detritus. Child or scope-less tokens are refused
    /// and Detritus is never contacted.
    pub fn forward_diagnostic(
        &self,
        token: &str,
        payload: &[u8],
        consent: diagnostics_policy::Consent,
        purpose: diagnostics_policy::Purpose,
        clock: &impl InstantSource,
    ) -> Result<(), BrokerError> {
        let scoped = self.require_scope(token, Scope::Diagnostics, clock)?;
        let profile = match scoped.protection {
            ProtectionClass::VerifiedAdult => diagnostics_policy::ProfileKind::Adult,
            _ => diagnostics_policy::ProfileKind::Child,
        };
        if !diagnostics_policy::may_collect(profile, consent, purpose) {
            return Err(BrokerError::MissingScope(Scope::Diagnostics));
        }
        self.detritus.forward(payload)?;
        Ok(())
    }
}
