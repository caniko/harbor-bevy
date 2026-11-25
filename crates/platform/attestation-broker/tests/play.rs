//! Play Age Signals attestation through a fake Google decode client, the
//! real OAuth/decode client over a fake HTTP exchange, and disabled routes.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use attestation_broker::{
    Broker, BrokerConfig, BrokerError, FakeDetritus, FakeSteamApi, GooglePlayIntegrity, HttpPost,
    HttpReply, InMemorySecrets, PlayIntegrityApi, Scope, ServiceAccount,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use platform_attestation::{
    AttestationPolicy, InstantSource, ProtectionClass, SteamTicket, play_request_hash,
};

const NOW: u64 = 1_700_000_000;
const TIER_C_ADULT: &str = "status=SHARED;lower=18;upper=;source=TIER_C;change=";
const TIER_A_ADULT: &str = "status=SHARED;lower=18;upper=;source=TIER_A;change=";
const UPPER_12: &str = "status=SHARED;lower=10;upper=12;source=TIER_A;change=";
const NOT_SHARED: &str = "status=NOT_SHARED;lower=;upper=;source=;change=";
const CALLER: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 7));

struct ClockAt(u64);
impl InstantSource for ClockAt {
    fn now_secs(&self) -> u64 {
        self.0
    }
}

fn secrets() -> InMemorySecrets {
    InMemorySecrets {
        steam_api_key: None,
        token_hmac_key: Some(b"test-hmac-key-32-bytes-long!!".to_vec()),
        detritus_bearer: Some("bearer".to_string()),
    }
}

fn config() -> BrokerConfig {
    BrokerConfig::new(1001, "http://127.0.0.1:4317".to_string(), policy())
}

fn policy() -> AttestationPolicy {
    AttestationPolicy::new("com.example.game", "example:play-age:v1")
}

/// Google's `decodeIntegrityToken` body for a genuine, licensed install.
fn verdict(package: &str, request_hash: &str, licensing: &str) -> String {
    serde_json::json!({
        "tokenPayloadExternal": {
            "requestDetails": {
                "requestPackageName": package,
                "requestHash": request_hash,
                "timestampMillis": (NOW * 1000).to_string(),
            },
            "appIntegrity": {"appRecognitionVerdict": "PLAY_RECOGNIZED"},
            "deviceIntegrity": {"deviceRecognitionVerdict": ["MEETS_DEVICE_INTEGRITY"]},
            "accountDetails": {"appLicensingVerdict": licensing},
        }
    })
    .to_string()
}

/// Fake Google client: answers with a verdict computed from the claim the
/// test is about to submit (Google echoes the app's requestHash).
struct FakeGoogle {
    reply: Arc<std::sync::Mutex<Result<String, BrokerError>>>,
    calls: Arc<AtomicUsize>,
}

impl PlayIntegrityApi for FakeGoogle {
    fn decode_integrity_token(
        &self,
        package: &str,
        token: &str,
        _now_secs: u64,
    ) -> Result<String, BrokerError> {
        assert_eq!(package, "com.example.game");
        assert_eq!(token, "opaque-integrity-token");
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.reply.lock().unwrap().clone()
    }
}

struct PlayHarness {
    broker: Broker<FakeSteamApi, FakeDetritus>,
    reply: Arc<std::sync::Mutex<Result<String, BrokerError>>>,
    calls: Arc<AtomicUsize>,
}

impl PlayHarness {
    fn new() -> Self {
        let reply = Arc::new(std::sync::Mutex::new(Err(BrokerError::Upstream(
            "unset".into(),
        ))));
        let calls = Arc::new(AtomicUsize::new(0));
        let steam = FakeSteamApi {
            response: Err(BrokerError::Upstream("unused".into())),
        };
        let broker = Broker::new(config(), &secrets(), steam, FakeDetritus::default())
            .expect("secrets complete")
            .with_play(FakeGoogle {
                reply: reply.clone(),
                calls: calls.clone(),
            });
        Self {
            broker,
            reply,
            calls,
        }
    }

    /// Issue a nonce, have "Google" bind `claim` to it, and attest.
    fn attest(
        &mut self,
        claim: &str,
        package: &str,
        licensing: &str,
    ) -> Result<attestation_broker::AttestedGrant, BrokerError> {
        let nonce = self
            .broker
            .issue_play_nonce(CALLER, &ClockAt(NOW))
            .expect("play route enabled");
        let hash = play_request_hash(&policy(), &nonce, claim);
        *self.reply.lock().unwrap() = Ok(verdict(package, &hash, licensing));
        self.broker
            .attest_play("opaque-integrity-token", claim, &nonce, &ClockAt(NOW))
    }
}

#[test]
fn play_age_classes_drive_scopes() {
    let cases = [
        (TIER_C_ADULT, ProtectionClass::VerifiedAdult, true),
        (TIER_A_ADULT, ProtectionClass::Unknown, false),
        (UPPER_12, ProtectionClass::ProtectedMinor, false),
        (NOT_SHARED, ProtectionClass::Unknown, false),
    ];
    for (claim, class, diagnostics) in cases {
        let mut play = PlayHarness::new();
        let grant = play
            .attest(claim, "com.example.game", "LICENSED")
            .expect("genuine verdict attests");
        assert_eq!(grant.protection, class, "{claim}");
        assert_eq!(
            grant.scopes.contains(&Scope::Diagnostics),
            diagnostics,
            "{claim}"
        );
        let scoped = play
            .broker
            .verify_token(&grant.token, &ClockAt(NOW))
            .expect("minted token verifies");
        assert_eq!(scoped.protection, class);
        assert!(scoped.scopes.contains(&Scope::Report));
    }
}

#[test]
fn adult_sources_follow_config() {
    let mut config = config();
    config.play_adult_sources = vec![platform_attestation::AgeRangeSource::TierA];
    let reply = Arc::new(std::sync::Mutex::new(Err(BrokerError::Expired)));
    let steam = FakeSteamApi {
        response: Err(BrokerError::Upstream("unused".into())),
    };
    let mut broker = Broker::new(config, &secrets(), steam, FakeDetritus::default())
        .unwrap()
        .with_play(FakeGoogle {
            reply: reply.clone(),
            calls: Arc::new(AtomicUsize::new(0)),
        });
    let nonce = broker.issue_play_nonce(CALLER, &ClockAt(NOW)).unwrap();
    *reply.lock().unwrap() = Ok(verdict(
        "com.example.game",
        &play_request_hash(&policy(), &nonce, TIER_A_ADULT),
        "LICENSED",
    ));
    let grant = broker
        .attest_play(
            "opaque-integrity-token",
            TIER_A_ADULT,
            &nonce,
            &ClockAt(NOW),
        )
        .expect("attests");
    assert_eq!(grant.protection, ProtectionClass::VerifiedAdult);
}

#[test]
fn replayed_and_unissued_nonces_never_reach_google() {
    let mut play = PlayHarness::new();
    let nonce = play.broker.issue_play_nonce(CALLER, &ClockAt(NOW)).unwrap();
    *play.reply.lock().unwrap() = Ok(verdict(
        "com.example.game",
        &play_request_hash(&policy(), &nonce, NOT_SHARED),
        "LICENSED",
    ));
    play.broker
        .attest_play("opaque-integrity-token", NOT_SHARED, &nonce, &ClockAt(NOW))
        .expect("first use attests");
    let err = play
        .broker
        .attest_play("opaque-integrity-token", NOT_SHARED, &nonce, &ClockAt(NOW))
        .expect_err("replayed nonce");
    assert_eq!(
        (err.clone(), err.http_status()),
        (BrokerError::Replayed, 409)
    );
    let err = play
        .broker
        .attest_play(
            "opaque-integrity-token",
            NOT_SHARED,
            "forged",
            &ClockAt(NOW),
        )
        .expect_err("unissued nonce");
    assert_eq!(err, BrokerError::Replayed);
    assert_eq!(play.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn expired_nonce_is_rejected() {
    let mut play = PlayHarness::new();
    let nonce = play.broker.issue_play_nonce(CALLER, &ClockAt(NOW)).unwrap();
    let late = ClockAt(NOW + platform_attestation::PLAY_NONCE_TTL_SECS);
    let err = play
        .broker
        .attest_play("opaque-integrity-token", NOT_SHARED, &nonce, &late)
        .expect_err("stale nonce");
    assert_eq!(err, BrokerError::Expired);
}

#[test]
fn verdict_failures_map_to_typed_statuses() {
    let mut play = PlayHarness::new();
    let err = play
        .attest(TIER_C_ADULT, "com.example.game.testpeer", "LICENSED")
        .expect_err("test peer package");
    assert_eq!(
        (err.clone(), err.http_status()),
        (BrokerError::WrongApp, 403)
    );
    let err = play
        .attest(TIER_C_ADULT, "com.example.game", "UNLICENSED")
        .expect_err("unlicensed");
    assert_eq!(
        (err.clone(), err.http_status()),
        (BrokerError::Untrusted, 403)
    );

    // A claim swapped after hashing (client upgrades itself to adult).
    let nonce = play.broker.issue_play_nonce(CALLER, &ClockAt(NOW)).unwrap();
    *play.reply.lock().unwrap() = Ok(verdict(
        "com.example.game",
        &play_request_hash(&policy(), &nonce, UPPER_12),
        "LICENSED",
    ));
    let err = play
        .broker
        .attest_play(
            "opaque-integrity-token",
            TIER_C_ADULT,
            &nonce,
            &ClockAt(NOW),
        )
        .expect_err("claim not bound by requestHash");
    assert_eq!(err.http_status(), 400);
}

#[test]
fn routes_without_credentials_are_disabled_not_fatal() {
    let steam: Option<FakeSteamApi> = None;
    let mut broker = Broker::new(config(), &secrets(), steam, FakeDetritus::default())
        .expect("missing steam key and service account do not refuse to serve");
    let err = broker
        .attest_steam(
            &SteamTicket {
                bytes: b"t".to_vec(),
                identity_hint: None,
            },
            None,
            &ClockAt(NOW),
        )
        .expect_err("steam disabled");
    assert_eq!(err.http_status(), 503);
    let err = broker
        .issue_play_nonce(CALLER, &ClockAt(NOW))
        .expect_err("play disabled");
    assert_eq!(err.http_status(), 503);
    let err = broker
        .attest_play("t", NOT_SHARED, "n", &ClockAt(NOW))
        .expect_err("play disabled");
    assert_eq!(err.http_status(), 503);
}

/// Records every POST and answers from a queue.
#[derive(Default)]
struct FakeHttp {
    calls: RefCell<Vec<(String, String, Option<String>, String)>>,
    replies: RefCell<Vec<HttpReply>>,
}

impl FakeHttp {
    fn reply(&self, status: u16, body: &str) {
        self.replies.borrow_mut().push(HttpReply {
            status,
            body: body.to_string(),
        });
    }
}

impl HttpPost for &FakeHttp {
    fn post(
        &self,
        url: &str,
        content_type: &str,
        bearer: Option<&str>,
        body: String,
    ) -> Result<HttpReply, BrokerError> {
        self.calls.borrow_mut().push((
            url.to_string(),
            content_type.to_string(),
            bearer.map(str::to_string),
            body,
        ));
        Ok(self.replies.borrow_mut().remove(0))
    }
}

fn account() -> ServiceAccount {
    ServiceAccount::with_signer(
        "broker@example-project.iam.gserviceaccount.com".to_string(),
        "key-1".to_string(),
        "https://oauth2.googleapis.com/token".to_string(),
        Box::new(|_message| Some(b"fake-signature".to_vec())),
    )
}

const OAUTH_OK: &str = r#"{"access_token":"access-1","expires_in":3599,"token_type":"Bearer"}"#;

#[test]
fn oauth_exchange_then_decode_with_cached_access_token() {
    let http = FakeHttp::default();
    http.reply(200, OAUTH_OK);
    http.reply(200, r#"{"tokenPayloadExternal":{}}"#);
    http.reply(200, r#"{"tokenPayloadExternal":{"second":true}}"#);
    let google = GooglePlayIntegrity::new(account(), &http);
    let body = google
        .decode_integrity_token("com.example.game", "tok-1", NOW)
        .expect("decodes");
    assert_eq!(body, r#"{"tokenPayloadExternal":{}}"#);
    google
        .decode_integrity_token("com.example.game", "tok-2", NOW + 60)
        .expect("decodes with the cached access token");

    let calls = http.calls.borrow();
    assert_eq!(calls.len(), 3, "one oauth exchange serves both decodes");
    let (url, content_type, bearer, form) = &calls[0];
    assert_eq!(url, "https://oauth2.googleapis.com/token");
    assert_eq!(content_type, "application/x-www-form-urlencoded");
    assert_eq!(bearer, &None);
    let jwt = form
        .strip_prefix("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=")
        .expect("jwt-bearer grant");
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
    let decode = |part: &str| -> serde_json::Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    };
    let header = decode(parts[0]);
    assert_eq!(header["alg"], "RS256");
    assert_eq!(header["kid"], "key-1");
    let claims = decode(parts[1]);
    assert_eq!(
        claims["iss"],
        "broker@example-project.iam.gserviceaccount.com"
    );
    assert_eq!(
        claims["scope"],
        "https://www.googleapis.com/auth/playintegrity"
    );
    assert_eq!(claims["aud"], "https://oauth2.googleapis.com/token");
    assert_eq!(claims["iat"], NOW);
    assert_eq!(claims["exp"], NOW + 3600);
    assert_eq!(parts[2], URL_SAFE_NO_PAD.encode(b"fake-signature"));

    let (url, content_type, bearer, body) = &calls[1];
    assert_eq!(
        url,
        "https://playintegrity.googleapis.com/v1/com.example.game:decodeIntegrityToken"
    );
    assert_eq!(content_type, "application/json");
    assert_eq!(bearer.as_deref(), Some("access-1"));
    assert_eq!(body, r#"{"integrity_token":"tok-1"}"#);
}

#[test]
fn google_failures_are_typed_and_redacted() {
    let http = FakeHttp::default();
    http.reply(500, "oauth down");
    let google = GooglePlayIntegrity::new(account(), &http);
    let err = google
        .decode_integrity_token("com.example.game", "secret-tok", NOW)
        .expect_err("oauth failure");
    assert!(matches!(err, BrokerError::Upstream(_)));

    http.reply(200, OAUTH_OK);
    http.reply(400, "bad token secret-tok");
    let err = google
        .decode_integrity_token("com.example.game", "secret-tok", NOW)
        .expect_err("rejected token");
    assert_eq!(err.http_status(), 400);
    assert!(!format!("{err} {err:?}").contains("secret-tok"));

    // An auth failure drops the cached access token: the next call
    // re-exchanges instead of reusing a revoked credential.
    http.reply(401, "expired");
    google
        .decode_integrity_token("com.example.game", "secret-tok", NOW)
        .expect_err("auth failure");
    http.reply(200, OAUTH_OK);
    http.reply(200, "{}");
    google
        .decode_integrity_token("com.example.game", "secret-tok", NOW)
        .expect("re-exchanges");
    let oauth_calls = http
        .calls
        .borrow()
        .iter()
        .filter(|call| call.0.contains("oauth2"))
        .count();
    assert_eq!(oauth_calls, 3);
    assert!(!format!("{google:?}").contains("access-1"));
}

#[test]
fn service_account_file_fails_closed() {
    for json in [
        "not json",
        r#"{"client_email":"a","private_key_id":"k","private_key":"x",
            "token_uri":"https://oauth2.googleapis.com/token"}"#,
        r#"{"client_email":"a","private_key_id":"k","private_key":"x",
            "token_uri":"http://plaintext.invalid/token"}"#,
    ] {
        let err = ServiceAccount::from_json(json).expect_err("invalid account");
        assert_eq!(
            err,
            BrokerError::Misconfigured("play integrity service account")
        );
    }
}

#[test]
fn one_flooding_address_cannot_exhaust_play_nonces() {
    let mut play = PlayHarness::new();
    let flooder = std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 9));
    let refused = (0..platform_attestation::IssuedNonces::MAX_OUTSTANDING)
        .find_map(|_| play.broker.issue_play_nonce(flooder, &ClockAt(NOW)).err())
        .expect("the flooding address is limited");
    assert_eq!(
        (refused.clone(), refused.http_status()),
        (BrokerError::RateLimited, 429)
    );
    play.broker
        .issue_play_nonce(CALLER, &ClockAt(NOW))
        .expect("another address is still served");
}
