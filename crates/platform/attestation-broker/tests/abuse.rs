use std::time::{Duration, UNIX_EPOCH};

use attestation_broker::{
    Broker, BrokerConfig, BrokerError, DeletionQueue, FakeDetritus, FakeSteamApi,
    FileDeletionQueue, FileReportSink, InMemorySecrets, MemoryDeletionQueue, MemoryReportSink,
    ReportSink, Scope,
};
use diagnostics_policy::{Consent, Purpose};
use platform_attestation::{AttestationPolicy, InstantSource, SteamTicket, VerifiedAgeSignal};

const NOW: u64 = 1_700_000_000;
const BEARER: &str = "detritus-bearer-secret";

struct ClockAt(u64);
impl InstantSource for ClockAt {
    fn now_secs(&self) -> u64 {
        self.0
    }
}

fn secrets() -> InMemorySecrets {
    InMemorySecrets {
        steam_api_key: Some("steam-key".to_string()),
        token_hmac_key: Some(b"test-hmac-key-32-bytes-long!!".to_vec()),
        detritus_bearer: Some(BEARER.to_string()),
    }
}

fn steam_ok() -> String {
    serde_json::json!({"response": {"params": {"result": "OK", "appid": 1001, "steamid": "76561198000000001"}}})
        .to_string()
}

fn steam_api(json: String) -> FakeSteamApi {
    FakeSteamApi { response: Ok(json) }
}

fn ticket() -> SteamTicket {
    SteamTicket {
        bytes: b"broker-test-ticket".to_vec(),
        identity_hint: None,
    }
}

fn config() -> BrokerConfig {
    BrokerConfig::new(
        1001,
        "http://127.0.0.1:4317".into(),
        AttestationPolicy::new("com.example.game", "example:play-age:v1"),
    )
}

fn broker() -> Broker<FakeSteamApi, FakeDetritus> {
    Broker::new(
        config(),
        &secrets(),
        steam_api(steam_ok()),
        FakeDetritus::default(),
    )
    .expect("test secrets complete")
}

fn signed_report(seed_byte: u8) -> trust_safety::SignedReport {
    let seed = [seed_byte; 32];
    trust_safety::sign_report_with_seed(
        trust_safety::SafetyReport {
            reason: trust_safety::ReportReason::AbusiveConduct,
            evidence: trust_safety::BoundedEvidence {
                peer_id: b"peer-1".to_vec(),
                match_id: None,
                mode: None,
                preset_message: None,
                timestamp: NOW,
            },
            reporter: trust_safety::PeerIdentity(b"reporter-1".to_vec()),
            target: trust_safety::PeerIdentity(b"peer-1".to_vec()),
            issued_at: NOW,
        },
        &seed,
    )
}

#[test]
fn client_report_json_survives_broker_parse_with_signature_intact() {
    // JSON in the exact shape the client sends (see safety::report_json):
    // signed match/preset context must parse back and still verify.
    let signed = trust_safety::sign_report_with_seed(
        trust_safety::SafetyReport {
            reason: trust_safety::ReportReason::CheatingOrExploit,
            evidence: trust_safety::BoundedEvidence {
                peer_id: b"peer-9".to_vec(),
                match_id: Some([9; 16]),
                mode: Some(1),
                preset_message: Some(trust_safety::PresetMessageId::Hello),
                timestamp: NOW,
            },
            reporter: trust_safety::PeerIdentity(b"reporter-9".to_vec()),
            target: trust_safety::PeerIdentity(b"peer-9".to_vec()),
            issued_at: NOW,
        },
        &[4; 32],
    );
    let dir = std::env::temp_dir().join(format!("cb_broker_json_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let json = serde_json::json!([{
        "subject": "76561198000000001",
        "reason": signed.report.reason as u8,
        "evidence": {
            "peer_id": signed.report.evidence.peer_id,
            "match_id": signed.report.evidence.match_id.map(|id| id.to_vec()),
            "mode": signed.report.evidence.mode,
            "preset_message": signed.report.evidence.preset_message.map(|id| id.to_u8()),
            "timestamp": signed.report.evidence.timestamp,
        },
        "reporter": signed.report.reporter.0,
        "target": signed.report.target.0,
        "issued_at": signed.report.issued_at,
        "signature": signed.signature.to_vec(),
        "public_key": signed.public_key.to_vec(),
    }]);
    for reason in [0, 6] {
        let mut invalid = json[0].clone();
        invalid["reason"] = reason.into();
        assert!(attestation_broker::parse_client_report(&invalid).is_none());
    }
    std::fs::write(dir.join("reports.json"), json.to_string()).unwrap();
    let sink = FileReportSink::open(dir.join("reports.json")).expect("loads");
    assert_eq!(sink.reports().len(), 1);
    let parsed = &sink.reports()[0];
    assert_eq!(parsed.subject, "76561198000000001");
    assert_eq!(parsed.report.report.evidence.match_id, Some([9; 16]));
    assert_eq!(parsed.report.report.evidence.mode, Some(1));
    assert_eq!(
        parsed.report.report.evidence.preset_message,
        Some(trust_safety::PresetMessageId::Hello)
    );
    trust_safety::verify_report(&parsed.report).expect("signature still verifies");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_queues_survive_reopen() {
    let dir = std::env::temp_dir().join(format!("cb_broker_reopen_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut sink = FileReportSink::open(dir.join("reports.json")).expect("opens empty");
        let mut queue = FileDeletionQueue::open(dir.join("deletions.json")).expect("opens empty");
        assert!(sink.store("sub-1", &signed_report(1)).expect("stores"));
        queue.enqueue("sub-1", NOW + 1).expect("enqueues");
        // Idempotent retries: same signature stores once, same subject upserts.
        assert!(!sink.store("sub-1", &signed_report(1)).expect("dedupes"));
        queue.enqueue("sub-1", NOW + 2).expect("upserts");
        assert_eq!(sink.reports().len(), 1);
        assert_eq!(queue.jobs().len(), 1);
        assert_eq!(queue.jobs()[0].deadline_unix_secs, NOW + 2);
    }
    let sink = FileReportSink::open(dir.join("reports.json")).expect("reopens");
    let queue = FileDeletionQueue::open(dir.join("deletions.json")).expect("reopens");
    assert_eq!(sink.reports().len(), 1);
    assert_eq!(sink.reports()[0].subject, "sub-1");
    trust_safety::verify_report(&sink.reports()[0].report).expect("verifies");
    assert_eq!(queue.jobs().len(), 1);
    assert_eq!(queue.jobs()[0].deadline_unix_secs, NOW + 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_stores_refuse_to_load() {
    let dir = std::env::temp_dir().join(format!("cb_broker_corrupt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("reports.json"), "[[[not valid").unwrap();
    std::fs::write(dir.join("deletions.json"), r#"[{"subject": 42}]"#).unwrap();
    assert!(FileReportSink::open(dir.join("reports.json")).is_err());
    assert!(FileDeletionQueue::open(dir.join("deletions.json")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn steam_attest_mints_report_delete_scopes() {
    let mut broker = broker();
    let token = broker
        .attest_steam(&ticket(), None, &ClockAt(NOW))
        .expect("ok fixture attests")
        .token;
    let scoped = broker
        .verify_token(&token, &ClockAt(NOW))
        .expect("fresh token verifies");
    assert!(scoped.scopes.contains(&Scope::Report));
    assert!(scoped.scopes.contains(&Scope::Delete));
    assert!(!scoped.scopes.contains(&Scope::Diagnostics));
}

#[test]
fn attest_grant_carries_broker_expiry_and_scopes() {
    let mut broker = broker();
    let grant = broker
        .attest_steam(&ticket(), None, &ClockAt(NOW))
        .expect("ok fixture attests");
    assert_eq!(grant.exp, NOW + 900);
    assert_eq!(grant.scopes, vec![Scope::Report, Scope::Delete]);
    assert!(!grant.scopes.contains(&Scope::Diagnostics));
}

#[test]
fn steam_reject_table_maps_to_4xx() {
    let cases = [
        (
            "expired",
            r#"{"response":{"params":{"result":"Expired","appid":1001,"steamid":"1"}}}"#,
            BrokerError::Expired,
            401,
        ),
        (
            "unsuccessful",
            r#"{"response":{"params":{"result":"Banned","appid":1001,"steamid":"1"}}}"#,
            BrokerError::Malformed("assertion failed verification".into()),
            400,
        ),
        (
            "wrong app",
            r#"{"response":{"params":{"result":"OK","appid":999,"steamid":"1"}}}"#,
            BrokerError::WrongApp,
            403,
        ),
        (
            "malformed",
            "not json",
            BrokerError::Malformed("assertion failed verification".into()),
            400,
        ),
    ];
    for (name, response, expected, status) in cases {
        let mut broker = Broker::new(
            config(),
            &secrets(),
            steam_api(response.to_string()),
            FakeDetritus::default(),
        )
        .expect("test secrets complete");
        let mut ticket = ticket();
        ticket.bytes.extend_from_slice(name.as_bytes());
        let err = broker
            .attest_steam(&ticket, None, &ClockAt(NOW))
            .expect_err(format!("{name} must fail").as_str());
        assert_eq!(err, expected, "{name}");
        assert_eq!(err.http_status(), status, "{name}");
    }
}

#[test]
fn steam_replay_rejected() {
    let mut broker = broker();
    broker
        .attest_steam(&ticket(), None, &ClockAt(NOW))
        .expect("first use succeeds");
    let err = broker
        .attest_steam(&ticket(), None, &ClockAt(NOW))
        .expect_err("second use is a replay");
    assert_eq!(err, BrokerError::Replayed);
    assert_eq!(err.http_status(), 409);
}

#[test]
fn expired_token_rejected() {
    let mut broker = broker();
    let token = broker
        .attest_steam(&ticket(), None, &ClockAt(NOW))
        .expect("ok fixture attests")
        .token;
    let err = broker
        .verify_token(&token, &ClockAt(NOW + 901))
        .expect_err("token past 15min ttl is invalid");
    assert_eq!(err, BrokerError::InvalidToken);
    assert_eq!(err.http_status(), 401);
}

#[test]
fn tampered_token_rejected() {
    let mut broker = broker();
    let token = broker
        .attest_steam(&ticket(), None, &ClockAt(NOW))
        .expect("ok fixture attests")
        .token;
    let tampered = format!("{token}tampered");
    let err = broker
        .verify_token(&tampered, &ClockAt(NOW))
        .expect_err("tampered token is invalid");
    assert_eq!(err, BrokerError::InvalidToken);
}

#[test]
fn reports_accepted_without_sanctions() {
    let mut broker = broker();
    let token = broker
        .attest_steam(&ticket(), None, &ClockAt(NOW))
        .expect("ok fixture attests")
        .token;
    let mut sink = MemoryReportSink::default();
    for seed in [1, 2, 3] {
        broker
            .submit_report(&token, &signed_report(seed), &mut sink, &ClockAt(NOW))
            .expect("each report accepted");
    }
    assert_eq!(sink.reports.len(), 3);
    // Accepted reports are bound to the authenticated token subject, not
    // just the client-asserted reporter inside the report.
    assert!(
        sink.reports
            .iter()
            .all(|stored| stored.subject == "76561198000000001")
    );
}

#[test]
fn bad_report_signature_rejected() {
    let mut broker = broker();
    let token = broker
        .attest_steam(&ticket(), None, &ClockAt(NOW))
        .expect("ok fixture attests")
        .token;
    let mut report = signed_report(1);
    report.signature[0] ^= 0xff;
    let mut sink = MemoryReportSink::default();
    let err = broker
        .submit_report(&token, &report, &mut sink, &ClockAt(NOW))
        .expect_err("forged report must fail");
    assert_eq!(err, BrokerError::BadReport);
    assert!(sink.reports.is_empty());
}

#[test]
fn deletion_accepted_with_30_day_deadline() {
    let mut broker = broker();
    let token = broker
        .attest_steam(&ticket(), None, &ClockAt(NOW))
        .expect("ok fixture attests")
        .token;
    let request = diagnostics_policy::DeleteRequest {
        now: UNIX_EPOCH + Duration::from_secs(NOW),
    };
    let mut queue = MemoryDeletionQueue::default();
    let deadline = broker
        .submit_deletion(&token, &request, &mut queue, &ClockAt(NOW))
        .expect("deletion accepted");
    assert_eq!(deadline, NOW + 30 * 86_400);
    assert_eq!(
        queue.jobs,
        vec![attestation_broker::DeletionJob {
            subject: "76561198000000001".to_string(),
            deadline_unix_secs: deadline,
        }]
    );
}

#[test]
fn child_diagnostics_forward_denied_without_touching_detritus() {
    let detritus = FakeDetritus::default();
    let mut broker = Broker::new(config(), &secrets(), steam_api(steam_ok()), detritus)
        .expect("test secrets complete");
    let token = broker
        .attest_steam(&ticket(), None, &ClockAt(NOW))
        .expect("unknown-protection token mints")
        .token;
    let err = broker
        .forward_diagnostic(
            &token,
            b"diagnostic bytes",
            Consent::Granted,
            Purpose::GameplayLogs,
            &ClockAt(NOW),
        )
        .expect_err("child/unknown diagnostics refused");
    assert_eq!(err, BrokerError::MissingScope(Scope::Diagnostics));
}

#[test]
fn adult_diagnostics_forward_allowed_then_withdrawn_refused() {
    let mut broker = broker();
    let token = broker
        .attest_steam(&ticket(), Some(VerifiedAgeSignal::Adult), &ClockAt(NOW))
        .expect("adult signal attests")
        .token;
    let scoped = broker
        .verify_token(&token, &ClockAt(NOW))
        .expect("token verifies");
    assert!(scoped.scopes.contains(&Scope::Diagnostics));
    broker
        .forward_diagnostic(
            &token,
            b"diagnostic bytes",
            Consent::Granted,
            Purpose::GameplayLogs,
            &ClockAt(NOW),
        )
        .expect("adult granted consent forwards");
    let err = broker
        .forward_diagnostic(
            &token,
            b"diagnostic bytes",
            Consent::Withdrawn,
            Purpose::GameplayLogs,
            &ClockAt(NOW),
        )
        .expect_err("withdrawn consent refused");
    assert_eq!(err, BrokerError::MissingScope(Scope::Diagnostics));
}

#[test]
fn missing_secrets_refuse_to_serve() {
    let no_hmac = InMemorySecrets {
        token_hmac_key: None,
        ..secrets()
    };
    let err = Broker::new(
        config(),
        &no_hmac,
        steam_api(steam_ok()),
        FakeDetritus::default(),
    )
    .expect_err("missing hmac key refuses");
    assert_eq!(err, BrokerError::Misconfigured("token hmac key"));

    let no_bearer = InMemorySecrets {
        detritus_bearer: None,
        ..secrets()
    };
    let err = Broker::new(
        config(),
        &no_bearer,
        steam_api(steam_ok()),
        FakeDetritus::default(),
    )
    .expect_err("missing detritus bearer refuses");
    assert_eq!(err, BrokerError::Misconfigured("detritus bearer"));
}

#[test]
fn detritus_bearer_never_leaks() {
    let mut broker = broker();
    let token = broker
        .attest_steam(&ticket(), None, &ClockAt(NOW))
        .expect("ok fixture attests")
        .token;
    assert!(!token.contains(BEARER));
    assert!(!format!("{broker:?}").contains(BEARER));
    let errors = [
        BrokerError::Malformed("probe".into()),
        BrokerError::Expired,
        BrokerError::WrongApp,
        BrokerError::WrongPlatform,
        BrokerError::Replayed,
        BrokerError::Untrusted,
        BrokerError::InvalidToken,
        BrokerError::MissingScope(Scope::Diagnostics),
        BrokerError::BadReport,
        BrokerError::Upstream("probe".into()),
        BrokerError::Misconfigured("probe"),
        BrokerError::Unavailable("probe"),
        BrokerError::Storage,
    ];
    for err in errors {
        assert!(!format!("{err}").contains(BEARER));
        assert!(!format!("{err:?}").contains(BEARER));
    }
}
