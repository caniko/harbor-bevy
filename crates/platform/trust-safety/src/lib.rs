//! Bevy-independent trust and safety core.

use std::borrow::Cow;
use std::collections::HashSet;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::{Rng, RngExt};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const ADJECTIVES: &[&str] = &[
    "Able", "Amber", "Bold", "Bright", "Calm", "Clever", "Eager", "Fair", "Fleet", "Gentle",
    "Keen", "Kind", "Nimble", "Quiet", "Swift", "Wise",
];
const NOUNS: &[&str] = &[
    "Anchor", "Beacon", "Cedar", "Comet", "Falcon", "Grove", "Harbor", "Lantern", "Maple",
    "Meadow", "Oak", "River", "Scout", "Spark", "Tower", "Willow",
];

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum PresetMessageId {
    Hello = 1,
    GoodLuck = 2,
    NiceMove = 3,
    WellPlayed = 4,
    OneMoment = 5,
    Ready = 6,
    Thanks = 7,
}

impl PresetMessageId {
    pub const ALL: [Self; 7] = [
        Self::Hello,
        Self::GoodLuck,
        Self::NiceMove,
        Self::WellPlayed,
        Self::OneMoment,
        Self::Ready,
        Self::Thanks,
    ];

    pub fn from_u8(value: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|id| *id as u8 == value)
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn catalog_id(self) -> &'static str {
        match self {
            Self::Hello => "mp.preset.hello",
            Self::GoodLuck => "mp.preset.good_luck",
            Self::NiceMove => "mp.preset.nice_move",
            Self::WellPlayed => "mp.preset.well_played",
            Self::OneMoment => "mp.preset.one_moment",
            Self::Ready => "mp.preset.ready",
            Self::Thanks => "mp.preset.thanks",
        }
    }

    pub fn en_source(self) -> &'static str {
        match self {
            Self::Hello => "Hello!",
            Self::GoodLuck => "Good luck!",
            Self::NiceMove => "Nice move!",
            Self::WellPlayed => "Well played!",
            Self::OneMoment => "One moment, please.",
            Self::Ready => "Ready!",
            Self::Thanks => "Thanks!",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewerKind {
    Child,
    Adult,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Alias(String);

impl Alias {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Alias {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

pub struct SubjectName<'a> {
    pub kind: ViewerKind,
    pub alias: &'a str,
    pub custom: Option<&'a str>,
}

pub fn generate_alias(rng: &mut impl Rng) -> Alias {
    let adjective = ADJECTIVES[rng.random_range(0..ADJECTIVES.len())];
    let noun = NOUNS[rng.random_range(0..NOUNS.len())];
    Alias(format!("{adjective}{noun}"))
}

pub fn display_name<'a>(viewer: ViewerKind, subject: SubjectName<'a>) -> Cow<'a, str> {
    if subject.kind == ViewerKind::Child || viewer == ViewerKind::Child {
        return Cow::Borrowed(subject.alias);
    }
    match subject.custom {
        Some(name) if !name.trim().is_empty() => Cow::Borrowed(name),
        _ => Cow::Borrowed(subject.alias),
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PeerIdentity(pub Vec<u8>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionGates {
    pub matching: bool,
    pub invites: bool,
    pub reconnects: bool,
    pub messages: bool,
    pub tournament_co_placement: bool,
}

impl ActionGates {
    fn allowed() -> Self {
        Self {
            matching: true,
            invites: true,
            reconnects: true,
            messages: true,
            tournament_co_placement: true,
        }
    }

    fn denied() -> Self {
        Self {
            matching: false,
            invites: false,
            reconnects: false,
            messages: false,
            tournament_co_placement: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct BlockList {
    blocked: HashSet<PeerIdentity>,
}

impl BlockList {
    pub fn insert(&mut self, id: PeerIdentity) {
        self.blocked.insert(id);
    }

    pub fn remove(&mut self, id: &PeerIdentity) -> bool {
        self.blocked.remove(id)
    }

    pub fn is_blocked(&self, id: &PeerIdentity) -> bool {
        self.blocked.contains(id)
    }

    pub fn identities(&self) -> impl Iterator<Item = &PeerIdentity> {
        self.blocked.iter()
    }

    pub fn apply_reviewed(&mut self, rules: impl IntoIterator<Item = DenyRule>) {
        for rule in rules {
            if rule.is_reviewed() {
                self.blocked.insert(rule.target);
            }
        }
    }

    pub fn gates(&self, id: &PeerIdentity) -> ActionGates {
        if self.is_blocked(id) {
            ActionGates::denied()
        } else {
            ActionGates::allowed()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ReportReason {
    AbusiveConduct = 1,
    CheatingOrExploit = 2,
    InappropriateIdentity = 3,
    Griefing = 4,
    SafetyConcern = 5,
}

impl ReportReason {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::AbusiveConduct),
            2 => Some(Self::CheatingOrExploit),
            3 => Some(Self::InappropriateIdentity),
            4 => Some(Self::Griefing),
            5 => Some(Self::SafetyConcern),
            _ => None,
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedEvidence {
    pub peer_id: Vec<u8>,
    pub match_id: Option<[u8; 16]>,
    pub mode: Option<u8>,
    pub preset_message: Option<PresetMessageId>,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafetyReport {
    pub reason: ReportReason,
    pub evidence: BoundedEvidence,
    pub reporter: PeerIdentity,
    pub target: PeerIdentity,
    pub issued_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedReport {
    pub report: SafetyReport,
    pub signature: [u8; 64],
    pub public_key: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportError {
    BadKey,
    BadSignature,
}

pub fn sign_report_with_seed(report: SafetyReport, seed: &[u8; 32]) -> SignedReport {
    sign_report(report, &SigningKey::from_bytes(seed))
}

pub fn sign_report(report: SafetyReport, signing_key: &SigningKey) -> SignedReport {
    let bytes = canonical_bytes(&report);
    let signature = signing_key.sign(&bytes);
    SignedReport {
        report,
        signature: signature.to_bytes(),
        public_key: signing_key.verifying_key().to_bytes(),
    }
}

pub fn verify_report(signed: &SignedReport) -> Result<(), ReportError> {
    let verifying =
        VerifyingKey::from_bytes(&signed.public_key).map_err(|_| ReportError::BadKey)?;
    let signature = Signature::from_bytes(&signed.signature);
    verifying
        .verify(&canonical_bytes(&signed.report), &signature)
        .map_err(|_| ReportError::BadSignature)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DenyRule {
    pub id: u64,
    pub target: PeerIdentity,
    reviewed: bool,
}

impl DenyRule {
    pub fn reviewed(id: u64, target: PeerIdentity) -> Self {
        Self {
            id,
            target,
            reviewed: true,
        }
    }

    pub fn is_reviewed(&self) -> bool {
        self.reviewed
    }
}

fn canonical_bytes(report: &SafetyReport) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.push(report.reason as u8);
    bytes.extend_from_slice(&report.issued_at.to_le_bytes());
    push_len_bytes(&mut bytes, &report.reporter.0);
    push_len_bytes(&mut bytes, &report.target.0);
    push_len_bytes(&mut bytes, &report.evidence.peer_id);
    match report.evidence.match_id {
        Some(id) => {
            bytes.push(1);
            bytes.extend_from_slice(&id);
        }
        None => bytes.push(0),
    }
    match report.evidence.mode {
        Some(mode) => bytes.extend_from_slice(&[1, mode]),
        None => bytes.push(0),
    }
    match report.evidence.preset_message {
        Some(id) => {
            bytes.push(1);
            bytes.push(id.to_u8());
        }
        None => bytes.push(0),
    }
    bytes.extend_from_slice(&report.evidence.timestamp.to_le_bytes());
    bytes
}

fn push_len_bytes(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_reasons_round_trip_and_reject_unknown() {
        let reasons = [
            ReportReason::AbusiveConduct,
            ReportReason::CheatingOrExploit,
            ReportReason::InappropriateIdentity,
            ReportReason::Griefing,
            ReportReason::SafetyConcern,
        ];
        for (index, reason) in reasons.into_iter().enumerate() {
            assert_eq!(reason.to_u8(), index as u8 + 1);
            assert_eq!(ReportReason::from_u8(reason.to_u8()), Some(reason));
        }
        assert_eq!(ReportReason::from_u8(0), None);
        assert_eq!(ReportReason::from_u8(6), None);
    }

    #[test]
    fn seven_preset_ids_round_trip() {
        assert_eq!(PresetMessageId::ALL.len(), 7);
        assert!(PresetMessageId::from_u8(0).is_none());
        assert!(PresetMessageId::from_u8(8).is_none());
        for id in PresetMessageId::ALL {
            assert_eq!(PresetMessageId::from_u8(id.to_u8()), Some(id));
            assert_ne!(id.catalog_id(), "");
            assert_ne!(id.en_source(), "");
        }
    }

    #[test]
    fn child_viewer_never_sees_custom_or_persona() {
        let alias = Alias("QuietOak".into());
        let shown = display_name(
            ViewerKind::Child,
            SubjectName {
                kind: ViewerKind::Adult,
                alias: alias.as_str(),
                custom: Some("can"),
            },
        );
        assert_eq!(shown, "QuietOak");
        assert_ne!(shown, "can");
        let steam = display_name(
            ViewerKind::Child,
            SubjectName {
                kind: ViewerKind::Adult,
                alias: alias.as_str(),
                custom: Some("SomeSteamPersona"),
            },
        );
        assert_eq!(steam, "QuietOak");
    }

    #[test]
    fn adult_to_adult_keeps_custom() {
        let shown = display_name(
            ViewerKind::Adult,
            SubjectName {
                kind: ViewerKind::Adult,
                alias: "QuietOak",
                custom: Some("can"),
            },
        );
        assert_eq!(shown, "can");
    }

    #[test]
    fn child_subject_always_alias() {
        let shown = display_name(
            ViewerKind::Adult,
            SubjectName {
                kind: ViewerKind::Child,
                alias: "SwiftRiver",
                custom: Some("can"),
            },
        );
        assert_eq!(shown, "SwiftRiver");
    }
}
