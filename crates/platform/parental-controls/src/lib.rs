//! Bevy-independent parental-controls core.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
    rand_core::{CryptoRng, RngCore},
};
use argon2::{Argon2, password_hash::rand_core::OsRng};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const MIN_PIN_DIGITS: usize = 4;
const MAX_FAILURES: usize = 5;
const LOCK_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PlatformGuidance {
    pub lock_enabled: bool,
    pub feature_community_blocked: bool,
    pub usable: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AgeBand {
    TenToTwelve,
    ThirteenPlus,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProtectionState {
    ChildOnlineDisabled,
    ChildOnlineParentEnabled,
    Adult,
    PendingLocalBand,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProtectedSettings {
    pub online_play: bool,
    pub social_features: bool,
    pub diagnostics: bool,
}

#[derive(Clone)]
pub struct PinHash(String);

impl std::fmt::Debug for PinHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PinHash(<redacted>)")
    }
}

impl PinHash {
    pub fn parse(phc: &str) -> Result<Self, PinError> {
        PasswordHash::new(phc).map_err(|_| PinError::HashFailed)?;
        Ok(Self(phc.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinError {
    InvalidPin,
    Mismatch,
    Locked,
    HashFailed,
    MissingPin,
    WrongState,
    DiagnosticsForbidden,
}

pub trait Clock {
    fn now(&self) -> SystemTime;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

#[derive(Clone, Debug, Default)]
pub struct AttemptLog {
    failures_ms: Vec<u64>,
}

impl AttemptLog {
    pub fn from_millis(failures_ms: Vec<u64>) -> Self {
        Self { failures_ms }
    }

    pub fn to_millis(&self) -> Vec<u64> {
        self.failures_ms.clone()
    }

    pub fn clear(&mut self) {
        self.failures_ms.clear();
    }

    fn prune(&mut self, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(LOCK_WINDOW.as_millis() as u64);
        self.failures_ms.retain(|&t| t > cutoff);
    }
}

pub trait PinVault {
    fn get(&self) -> Option<PinHash>;
    fn set(&mut self, hash: PinHash);
    fn clear(&mut self);
}

#[derive(Clone, Debug, Default)]
pub struct MemoryVault(Option<PinHash>);

impl PinVault for MemoryVault {
    fn get(&self) -> Option<PinHash> {
        self.0.clone()
    }

    fn set(&mut self, hash: PinHash) {
        self.0 = Some(hash);
    }

    fn clear(&mut self) {
        self.0 = None;
    }
}

pub fn from_platform(guidance: PlatformGuidance) -> ProtectionState {
    if !guidance.usable {
        return ProtectionState::PendingLocalBand;
    }
    if guidance.lock_enabled || guidance.feature_community_blocked {
        ProtectionState::ChildOnlineDisabled
    } else {
        ProtectionState::Adult
    }
}

pub fn from_local_band(band: AgeBand) -> ProtectionState {
    match band {
        AgeBand::TenToTwelve => ProtectionState::ChildOnlineDisabled,
        AgeBand::ThirteenPlus => ProtectionState::Adult,
    }
}

impl ProtectionState {
    pub fn age_band(self) -> AgeBand {
        match self {
            Self::Adult => AgeBand::ThirteenPlus,
            Self::ChildOnlineDisabled | Self::ChildOnlineParentEnabled | Self::PendingLocalBand => {
                AgeBand::TenToTwelve
            }
        }
    }

    pub fn online_allowed(self, settings: &ProtectedSettings) -> bool {
        match self {
            Self::Adult => true,
            Self::ChildOnlineParentEnabled => settings.online_play,
            Self::ChildOnlineDisabled | Self::PendingLocalBand => false,
        }
    }

    pub fn diagnostics_allowed(self, settings: &ProtectedSettings) -> bool {
        self == Self::Adult && settings.diagnostics
    }
}

pub fn set_pin(pin: &str, mut rng: impl CryptoRng + RngCore) -> Result<PinHash, PinError> {
    validate_pin(pin)?;
    let salt = SaltString::generate(&mut rng);
    let hash = Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .map_err(|_| PinError::HashFailed)?;
    Ok(PinHash(hash.to_string()))
}

pub fn hash_pin(pin: &str) -> Result<PinHash, PinError> {
    set_pin(pin, OsRng)
}

pub fn verify_pin(
    pin: &str,
    hash: &PinHash,
    clock: &impl Clock,
    attempt_log: &mut AttemptLog,
) -> Result<(), PinError> {
    let now_ms = system_time_ms(clock.now());
    attempt_log.prune(now_ms);
    if attempt_log.failures_ms.len() >= MAX_FAILURES {
        return Err(PinError::Locked);
    }
    let parsed = PasswordHash::new(hash.as_str()).map_err(|_| PinError::HashFailed)?;
    match Argon2::default().verify_password(pin.as_bytes(), &parsed) {
        Ok(()) => {
            attempt_log.clear();
            Ok(())
        }
        Err(_) => {
            attempt_log.failures_ms.push(now_ms);
            Err(PinError::Mismatch)
        }
    }
}

pub fn enable_online(
    state: ProtectionState,
    settings: &mut ProtectedSettings,
    pin: &str,
    hash: &PinHash,
    clock: &impl Clock,
    attempt_log: &mut AttemptLog,
) -> Result<ProtectionState, PinError> {
    match state {
        ProtectionState::Adult => {
            settings.online_play = true;
            Ok(ProtectionState::Adult)
        }
        ProtectionState::ChildOnlineDisabled | ProtectionState::ChildOnlineParentEnabled => {
            verify_pin(pin, hash, clock, attempt_log)?;
            settings.online_play = true;
            Ok(ProtectionState::ChildOnlineParentEnabled)
        }
        ProtectionState::PendingLocalBand => Err(PinError::WrongState),
    }
}

pub fn disable_online(state: ProtectionState, settings: &mut ProtectedSettings) -> ProtectionState {
    settings.online_play = false;
    settings.social_features = false;
    match state {
        ProtectionState::Adult => ProtectionState::Adult,
        ProtectionState::ChildOnlineParentEnabled
        | ProtectionState::ChildOnlineDisabled
        | ProtectionState::PendingLocalBand => ProtectionState::ChildOnlineDisabled,
    }
}

pub fn set_diagnostics(
    state: ProtectionState,
    settings: &mut ProtectedSettings,
    enabled: bool,
) -> Result<(), PinError> {
    if state != ProtectionState::Adult {
        return Err(PinError::DiagnosticsForbidden);
    }
    settings.diagnostics = enabled;
    Ok(())
}

pub fn reset_forgotten_pin(
    vault: &mut impl PinVault,
    settings: &mut ProtectedSettings,
    attempt_log: &mut AttemptLog,
) -> ProtectionState {
    vault.clear();
    *settings = ProtectedSettings::default();
    attempt_log.clear();
    ProtectionState::ChildOnlineDisabled
}

fn validate_pin(pin: &str) -> Result<(), PinError> {
    if pin.len() < MIN_PIN_DIGITS || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PinError::InvalidPin);
    }
    Ok(())
}

fn system_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
