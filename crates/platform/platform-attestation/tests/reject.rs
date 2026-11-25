use platform_attestation::{
    AgeRangeSource, AgeSignalsStatus, AppBinding, AttestationPolicy, InstantSource, IssuedNonces,
    MemoryNonceCache, NonceRefusal, PLAY_NONCE_TTL_SECS, Platform, PlayAgeClaim, ProtectionClass,
    SteamTicket, VerifiedAgeSignal, VerifyError, play_request_hash, verify_play, verify_steam,
};

const NOW: u64 = 1_700_000_000;
const NONCE: &str = "test-nonce-1";

struct FixedClock;
impl InstantSource for FixedClock {
    fn now_secs(&self) -> u64 {
        NOW
    }
}

fn steam_response(result: &str, appid: Option<u32>, steamid: Option<&str>) -> String {
    let mut params = serde_json::Map::new();
    params.insert(
        "result".to_string(),
        serde_json::Value::String(result.to_string()),
    );
    if let Some(id) = appid {
        params.insert("appid".to_string(), serde_json::Value::Number(id.into()));
    }
    if let Some(id) = steamid {
        params.insert(
            "steamid".to_string(),
            serde_json::Value::String(id.to_string()),
        );
    }
    serde_json::Value::Object({
        let mut response = serde_json::Map::new();
        response.insert(
            "response".to_string(),
            serde_json::Value::Object({
                let mut outer = serde_json::Map::new();
                outer.insert("params".to_string(), serde_json::Value::Object(params));
                outer
            }),
        );
        response
    })
    .to_string()
}

fn ticket() -> SteamTicket {
    SteamTicket {
        bytes: b"test-ticket-bytes".to_vec(),
        identity_hint: None,
    }
}

fn binding() -> AppBinding {
    AppBinding::new(Some(1001))
}

#[test]
fn steam_success_yields_ephemeral_assertion() {
    let mut cache = MemoryNonceCache::default();
    let assertion = verify_steam(
        &binding(),
        &steam_response("OK", Some(1001), Some("76561198000000000")),
        &ticket(),
        None,
        &mut cache,
        &FixedClock,
    )
    .expect("success fixture verifies");
    assert_eq!(assertion.platform, Platform::Steam);
    assert_eq!(assertion.app_id, "steam:1001");
    assert_eq!(assertion.subject, "76561198000000000");
    assert_eq!(assertion.protection, ProtectionClass::Unknown);
    assert!(assertion.expires_at > NOW);
}

#[test]
fn steam_reject_table() {
    let cases = [
        (
            "expired result",
            steam_response("Expired", Some(1001), Some("1")),
            VerifyError::Expired,
        ),
        (
            "malformed json",
            "not json".to_string(),
            VerifyError::Malformed,
        ),
        (
            "unsuccessful result",
            steam_response("Banned", Some(1001), Some("1")),
            VerifyError::Malformed,
        ),
        (
            "wrong appid",
            steam_response("OK", Some(999), Some("1")),
            VerifyError::WrongApp,
        ),
        (
            "missing appid",
            steam_response("OK", None, Some("1")),
            VerifyError::Malformed,
        ),
        (
            "missing steamid",
            steam_response("OK", Some(1001), None),
            VerifyError::Malformed,
        ),
    ];
    for (name, response, expected) in cases {
        let mut cache = MemoryNonceCache::default();
        let mut ticket = ticket();
        ticket.bytes.extend_from_slice(name.as_bytes());
        let err = verify_steam(
            &binding(),
            &response,
            &ticket,
            None,
            &mut cache,
            &FixedClock,
        )
        .expect_err(format!("{name} must fail").as_str());
        assert_eq!(err, expected, "{name}");
    }
}

#[test]
fn steam_missing_binding_is_wrong_app() {
    let mut cache = MemoryNonceCache::default();
    let err = verify_steam(
        &AppBinding::default(),
        &steam_response("OK", Some(1001), Some("1")),
        &ticket(),
        None,
        &mut cache,
        &FixedClock,
    )
    .expect_err("missing steam_app_id must fail");
    assert_eq!(err, VerifyError::WrongApp);
}

#[test]
fn steam_replay_rejected() {
    let mut cache = MemoryNonceCache::default();
    let response = steam_response("OK", Some(1001), Some("1"));
    verify_steam(
        &binding(),
        &response,
        &ticket(),
        None,
        &mut cache,
        &FixedClock,
    )
    .expect("first use succeeds");
    let err = verify_steam(
        &binding(),
        &response,
        &ticket(),
        None,
        &mut cache,
        &FixedClock,
    )
    .expect_err("second use is a replay");
    assert_eq!(err, VerifyError::Replayed);
}

#[test]
fn age_signal_maps_to_protection_class() {
    let response = steam_response("OK", Some(1001), Some("1"));
    let mut cache = MemoryNonceCache::default();
    let adult = verify_steam(
        &binding(),
        &response,
        &ticket(),
        Some(VerifiedAgeSignal::Adult),
        &mut cache,
        &FixedClock,
    )
    .expect("adult signal verifies");
    assert_eq!(adult.protection, ProtectionClass::VerifiedAdult);

    let mut cache = MemoryNonceCache::default();
    let mut other_ticket = ticket();
    other_ticket.bytes.extend_from_slice(b"minor");
    let minor = verify_steam(
        &binding(),
        &response,
        &other_ticket,
        Some(VerifiedAgeSignal::SupervisedMinor),
        &mut cache,
        &FixedClock,
    )
    .expect("minor signal verifies");
    assert_eq!(minor.protection, ProtectionClass::ProtectedMinor);
}

#[test]
fn raw_material_never_logged() {
    let ticket = SteamTicket {
        bytes: b"super-secret-ticket".to_vec(),
        identity_hint: None,
    };
    assert!(!format!("{ticket:?}").contains("super-secret"));
}

const RELEASE: &str = "com.example.game";
const DOMAIN: &str = "example:play-age:v1";
const NOT_SHARED: &str = "status=NOT_SHARED;lower=;upper=;source=;change=";
const TIER_A_ADULT: &str = "status=SHARED;lower=18;upper=;source=TIER_A;change=";
const TIER_C_ADULT: &str = "status=SHARED;lower=18;upper=;source=TIER_C;change=";
const TIER_D_ADULT: &str = "status=SHARED;lower=18;upper=;source=TIER_D;change=";
const UPPER_12: &str = "status=SHARED;lower=10;upper=12;source=TIER_A;change=";
const TIER_B_MINOR: &str = "status=SHARED;lower=16;upper=18;source=TIER_B;change=APPROVED";

fn policy() -> AttestationPolicy {
    AttestationPolicy::new(RELEASE, DOMAIN)
}

/// Fixture `decodeIntegrityToken` response. Overrides replace one field.
struct Verdict {
    package: &'static str,
    hash: String,
    timestamp_ms: u64,
    app: &'static str,
    device: Vec<&'static str>,
    licensing: &'static str,
}

impl Verdict {
    fn genuine(claim: &str) -> Self {
        Self {
            package: RELEASE,
            hash: play_request_hash(&policy(), NONCE, claim),
            timestamp_ms: NOW * 1000,
            app: "PLAY_RECOGNIZED",
            device: vec!["MEETS_BASIC_INTEGRITY", "MEETS_DEVICE_INTEGRITY"],
            licensing: "LICENSED",
        }
    }

    fn json(&self) -> String {
        serde_json::json!({
            "tokenPayloadExternal": {
                "requestDetails": {
                    "requestPackageName": self.package,
                    "requestHash": self.hash,
                    // Google sends int64 fields as strings.
                    "timestampMillis": self.timestamp_ms.to_string(),
                },
                "appIntegrity": {
                    "appRecognitionVerdict": self.app,
                    "packageName": self.package,
                    "versionCode": "1",
                },
                "deviceIntegrity": {"deviceRecognitionVerdict": self.device},
                "accountDetails": {"appLicensingVerdict": self.licensing},
            }
        })
        .to_string()
    }
}

fn play(verdict: &Verdict, claim: &str) -> Result<ProtectionClass, VerifyError> {
    verify_play(
        &policy(),
        &verdict.json(),
        NONCE,
        claim,
        AgeRangeSource::ID_CHECKED,
        &FixedClock,
    )
    .map(|assertion| {
        assert_eq!(assertion.platform, Platform::Play);
        assert_eq!(assertion.app_id, "play:com.example.game");
        assert!(assertion.expires_at > NOW);
        assertion.protection
    })
}

#[test]
fn play_reject_table() {
    let claim = TIER_C_ADULT;
    let genuine = || Verdict::genuine(claim);
    let cases = [
        (
            "wrong package",
            Verdict {
                package: "com.example.game.testpeer",
                ..genuine()
            },
            VerifyError::WrongApp,
        ),
        (
            "wrong hash",
            Verdict {
                hash: play_request_hash(&policy(), NONCE, TIER_D_ADULT),
                ..genuine()
            },
            VerifyError::Malformed,
        ),
        (
            "hash for another nonce",
            Verdict {
                hash: play_request_hash(&policy(), "other-nonce", claim),
                ..genuine()
            },
            VerifyError::Malformed,
        ),
        (
            "hash for another domain",
            Verdict {
                hash: play_request_hash(
                    &AttestationPolicy::new(RELEASE, "example:play-age:v2"),
                    NONCE,
                    claim,
                ),
                ..genuine()
            },
            VerifyError::Malformed,
        ),
        (
            "stale timestamp",
            Verdict {
                timestamp_ms: (NOW - 121) * 1000,
                ..genuine()
            },
            VerifyError::Expired,
        ),
        (
            "future timestamp",
            Verdict {
                timestamp_ms: (NOW + 61) * 1000,
                ..genuine()
            },
            VerifyError::Malformed,
        ),
        (
            "unrecognized app",
            Verdict {
                app: "UNRECOGNIZED_VERSION",
                ..genuine()
            },
            VerifyError::WrongApp,
        ),
        (
            "no device integrity",
            Verdict {
                device: vec!["MEETS_BASIC_INTEGRITY"],
                ..genuine()
            },
            VerifyError::Untrusted,
        ),
        (
            "unlicensed",
            Verdict {
                licensing: "UNLICENSED",
                ..genuine()
            },
            VerifyError::Untrusted,
        ),
    ];
    for (name, verdict, expected) in cases {
        assert_eq!(play(&verdict, claim), Err(expected), "{name}");
    }
}

#[test]
fn play_malformed_verdicts_fail() {
    for json in ["not json", "{}", r#"{"tokenPayloadExternal":{}}"#] {
        let err = verify_play(
            &policy(),
            json,
            NONCE,
            NOT_SHARED,
            AgeRangeSource::ID_CHECKED,
            &FixedClock,
        );
        assert_eq!(err, Err(VerifyError::Malformed), "{json}");
    }
}

#[test]
fn play_age_mapping_table() {
    let cases = [
        ("not shared", NOT_SHARED, ProtectionClass::Unknown),
        (
            "age signals failure",
            "status=UNAVAILABLE;lower=;upper=;source=;change=",
            ProtectionClass::Unknown,
        ),
        (
            "verification required",
            "status=VERIFICATION_REQUIRED;lower=;upper=;source=;change=",
            ProtectionClass::Unknown,
        ),
        ("tier A adult", TIER_A_ADULT, ProtectionClass::Unknown),
        ("tier C adult", TIER_C_ADULT, ProtectionClass::VerifiedAdult),
        ("tier D adult", TIER_D_ADULT, ProtectionClass::VerifiedAdult),
        ("upper 12", UPPER_12, ProtectionClass::ProtectedMinor),
        (
            "tier B minor",
            TIER_B_MINOR,
            ProtectionClass::ProtectedMinor,
        ),
        (
            "tier C 16-18 is not proven adult",
            "status=SHARED;lower=16;upper=18;source=TIER_C;change=",
            ProtectionClass::Unknown,
        ),
        (
            "shared without ages",
            "status=SHARED;lower=;upper=;source=;change=",
            ProtectionClass::Unknown,
        ),
    ];
    for (name, claim, expected) in cases {
        assert_eq!(
            play(&Verdict::genuine(claim), claim),
            Ok(expected),
            "{name}"
        );
    }
}

#[test]
fn adult_sources_are_configurable() {
    let claim = TIER_A_ADULT;
    let with_tier_a = verify_play(
        &policy(),
        &Verdict::genuine(claim).json(),
        NONCE,
        claim,
        &[AgeRangeSource::TierA],
        &FixedClock,
    );
    assert_eq!(
        with_tier_a.map(|a| a.protection),
        Ok(ProtectionClass::VerifiedAdult)
    );
    let none = verify_play(
        &policy(),
        &Verdict::genuine(TIER_C_ADULT).json(),
        NONCE,
        TIER_C_ADULT,
        &[],
        &FixedClock,
    );
    assert_eq!(none.map(|a| a.protection), Ok(ProtectionClass::Unknown));
}

#[test]
fn play_verdict_age_window_boundary() {
    let claim = NOT_SHARED;
    let edge = Verdict {
        timestamp_ms: (NOW - 120) * 1000,
        ..Verdict::genuine(claim)
    };
    assert_eq!(play(&edge, claim), Ok(ProtectionClass::Unknown));
}

#[test]
fn claims_parse_only_in_canonical_form() {
    let claim = PlayAgeClaim::parse(TIER_B_MINOR).expect("canonical claim parses");
    assert_eq!(claim.status, AgeSignalsStatus::Shared);
    assert_eq!((claim.age_lower, claim.age_upper), (Some(16), Some(18)));
    assert_eq!(claim.canonical(), TIER_B_MINOR);
    for bad in [
        "",
        "status=SHARED",
        "status=SHARED;lower=018;upper=;source=TIER_C;change=",
        "status=SHARED;lower=19;upper=;source=TIER_C;change=",
        "status=SHARED;lower=15;upper=12;source=TIER_A;change=",
        "status=SHARED;lower=18;upper=;source=TIER_Z;change=",
        "status=shared;lower=;upper=;source=;change=",
        "status=SHARED;upper=;lower=;source=;change=",
        "status=NOT_SHARED;lower=;upper=;source=;change=;extra=1",
    ] {
        assert_eq!(PlayAgeClaim::parse(bad), None, "{bad}");
    }
    // A malformed claim with a matching hash still fails verification.
    let bad = "status=SHARED;lower=018;upper=;source=TIER_C;change=";
    assert_eq!(
        play(&Verdict::genuine(bad), bad),
        Err(VerifyError::Malformed)
    );
}

#[test]
fn request_hash_is_stable() {
    use base64::Engine as _;
    // Pinned with coreutils: printf 'example:play-age:v1\nn\n<claim>' | sha256sum.
    // The Android bridge hashes the same bytes.
    let hash = play_request_hash(&policy(), "n", NOT_SHARED);
    assert_eq!(hash.len(), 43, "unpadded base64url SHA-256");
    let digest = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&hash)
        .expect("base64url");
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex,
        "3af46672ac98a5bf8fcdf1d26f49a7dd04b33ba37b89a096eb4a9792768026df"
    );
    assert_ne!(hash, play_request_hash(&policy(), "m", NOT_SHARED));
    let spaced = AttestationPolicy::new(RELEASE, format!("{DOMAIN} "));
    assert_ne!(hash, play_request_hash(&spaced, "n", NOT_SHARED));
}

fn ip(text: &str) -> std::net::IpAddr {
    text.parse().expect("ip literal")
}

#[test]
fn nonces_are_single_use_and_expire() {
    let caller = ip("198.51.100.7");
    let mut nonces = IssuedNonces::default();
    assert_eq!(nonces.insert(NONCE.to_string(), caller, NOW), Ok(()));
    assert_eq!(nonces.consume(NONCE, NOW), Ok(()));
    assert_eq!(nonces.consume(NONCE, NOW), Err(VerifyError::Replayed));
    assert_eq!(
        nonces.consume("never-issued", NOW),
        Err(VerifyError::Replayed)
    );
    assert_eq!(nonces.insert("late".to_string(), caller, NOW), Ok(()));
    assert_eq!(
        nonces.consume("late", NOW + PLAY_NONCE_TTL_SECS),
        Err(VerifyError::Expired)
    );
}

#[test]
fn nonce_issuing_is_capped() {
    let mut nonces = IssuedNonces::default();
    for i in 0..IssuedNonces::MAX_OUTSTANDING {
        let caller = std::net::IpAddr::V4((0x0a00_0000 + i as u32).into());
        assert_eq!(nonces.insert(i.to_string(), caller, NOW), Ok(()));
    }
    assert_eq!(
        nonces.insert("one-too-many".to_string(), ip("192.0.2.1"), NOW),
        Err(NonceRefusal::Capacity)
    );
    // Expired entries free capacity.
    assert_eq!(
        nonces.insert(
            "later".to_string(),
            ip("192.0.2.1"),
            NOW + PLAY_NONCE_TTL_SECS
        ),
        Ok(())
    );
}

#[test]
fn a_flooding_caller_cannot_starve_another() {
    let mut nonces = IssuedNonces::default();
    let flooder = ip("203.0.113.9");
    let issued = (0..IssuedNonces::MAX_OUTSTANDING)
        .filter(|i| nonces.insert(format!("flood-{i}"), flooder, NOW).is_ok())
        .count();
    assert_eq!(issued, IssuedNonces::MAX_OUTSTANDING_PER_CALLER);
    assert_eq!(
        nonces.insert("more".to_string(), flooder, NOW),
        Err(NonceRefusal::CallerLimited)
    );
    assert_eq!(
        nonces.insert("legit".to_string(), ip("198.51.100.7"), NOW),
        Ok(())
    );
    assert_eq!(nonces.consume("legit", NOW), Ok(()));

    // Rotating addresses inside one IPv6 /64 is one caller.
    let v6 = (0..=IssuedNonces::MAX_OUTSTANDING_PER_CALLER)
        .filter(|i| {
            let addr = ip(&format!("2001:db8:1:2::{:x}", i + 1));
            nonces.insert(format!("v6-{i}"), addr, NOW).is_ok()
        })
        .count();
    assert_eq!(v6, IssuedNonces::MAX_OUTSTANDING_PER_CALLER);
}

#[test]
fn spending_nonces_does_not_lift_the_caller_rate() {
    let mut nonces = IssuedNonces::default();
    let caller = ip("198.51.100.20");
    for i in 0..IssuedNonces::MAX_ISSUED_PER_CALLER_WINDOW {
        assert_eq!(nonces.insert(i.to_string(), caller, NOW), Ok(()));
        assert_eq!(nonces.consume(&i.to_string(), NOW), Ok(()));
    }
    assert_eq!(
        nonces.insert("burst".to_string(), caller, NOW),
        Err(NonceRefusal::CallerLimited)
    );
    let next_window = NOW + IssuedNonces::CALLER_WINDOW_SECS;
    assert_eq!(
        nonces.insert("next".to_string(), caller, next_window),
        Ok(())
    );
}
