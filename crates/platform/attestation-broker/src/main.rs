//! `attestation-broker` binary (feature `bin`).
//!
//! HTTP/1.1 server over Hyper and Tokio.

use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::sync::Arc;

use attestation_broker::{
    AttestedGrant, Broker, BrokerConfig, BrokerError, FileDeletionQueue, FileReportSink,
    GooglePlayIntegrity, HttpReply, SecretStore, ServiceAccount,
};
use clap::Parser;
use platform_attestation::{AgeRangeSource, AttestationPolicy, InstantSource as _, SteamTicket};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: String,
    #[arg(long)]
    steam_app_id: u32,
    #[arg(long, env = "ATTESTATION_PLAY_PACKAGE", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    play_package: String,
    #[arg(long, env = "ATTESTATION_PLAY_AGE_HASH_DOMAIN", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    play_age_hash_domain: String,
    #[arg(long, default_value_t = 900)]
    token_ttl_secs: u64,
    #[arg(long)]
    detritus_url: String,
    #[arg(long, default_value = "./broker-data")]
    data_dir: String,
    /// Age Signals sources (comma-separated TIER_A..TIER_D) that may prove
    /// an adult. Default: ID-checked tiers only.
    #[arg(long, default_value = "TIER_C,TIER_D")]
    play_adult_age_sources: String,
    /// Header a trusted reverse proxy sets to the client address (e.g.
    /// X-Forwarded-For). Unset: rate limits key on the socket peer, and any
    /// such header is ignored. Set it only behind a proxy that overwrites or
    /// appends it; the rightmost entry of the last header line is used.
    #[arg(long)]
    client_ip_header: Option<String>,
}

struct EnvSecrets {
    steam_api_key: Option<String>,
    token_hmac_key: Option<Vec<u8>>,
    detritus_bearer: Option<String>,
}

impl SecretStore for EnvSecrets {
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

fn secret_bytes(name: &str) -> Result<Option<Vec<u8>>, std::io::Error> {
    if let Some(path) = std::env::var_os(format!("{name}_FILE")) {
        return std::fs::read(path).map(Some);
    }
    Ok(std::env::var(name).ok().map(String::into_bytes))
}

fn env_or_file(name: &str) -> Result<Option<String>, std::io::Error> {
    secret_bytes(name)?
        .map(|bytes| {
            String::from_utf8(bytes)
                .map(|s| s.trim().to_string())
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))
        })
        .transpose()
}

fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

struct ReqwestSteamApi {
    client: reqwest::Client,
    api_key: String,
    app_id: u32,
}

impl attestation_broker::SteamWebApi for ReqwestSteamApi {
    fn authenticate_user_ticket(&self, ticket: &SteamTicket) -> Result<String, BrokerError> {
        // ponytail: sync shell over async client; broker throughput is low.
        // Upstream errors map to a fixed string: reqwest errors can echo the
        // request URL, which carries the publisher key and ticket.
        // Query string is built by hand: this reqwest feature set has no
        // `.query()` helper. The key is percent-encoded so error redaction
        // below is not the only layer keeping it out of logs.
        let url = format!(
            "https://api.steampowered.com/ISteamUser/AuthenticateUserTicket/v1/?key={}&appid={}&ticket={}",
            percent_encode(&self.api_key),
            self.app_id,
            hex_encode(&ticket.bytes)
        );
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                self.client
                    .get(&url)
                    .send()
                    .await
                    .map_err(|_| BrokerError::Upstream("steam api request failed".into()))?
                    .text()
                    .await
                    .map_err(|_| BrokerError::Upstream("steam api request failed".into()))
            })
        })
    }
}

struct ReqwestDetritus {
    client: reqwest::Client,
    url: String,
    bearer: String,
}

impl attestation_broker::DetritusClient for ReqwestDetritus {
    fn forward(&self, payload: &[u8]) -> Result<(), BrokerError> {
        let payload = payload.to_vec();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let response = self
                    .client
                    .post(&self.url)
                    .bearer_auth(&self.bearer)
                    .body(payload)
                    .send()
                    .await
                    .map_err(|_| BrokerError::Upstream("detritus forward failed".into()))?;
                if response.status().is_success() {
                    Ok(())
                } else {
                    Err(BrokerError::Upstream("detritus forward refused".into()))
                }
            })
        })
    }
}

/// HTTPS POST for the Google OAuth and decode calls. Errors are fixed
/// strings: reqwest errors can echo URLs and the body carries the token.
struct ReqwestHttp {
    client: reqwest::Client,
}

impl attestation_broker::HttpPost for ReqwestHttp {
    fn post(
        &self,
        url: &str,
        content_type: &str,
        bearer: Option<&str>,
        body: String,
    ) -> Result<HttpReply, BrokerError> {
        let failed = || BrokerError::Upstream("play integrity request failed".into());
        let mut request = self
            .client
            .post(url)
            .header("content-type", content_type)
            .body(body);
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let response = request.send().await.map_err(|_| failed())?;
                let status = response.status().as_u16();
                let body = response.text().await.map_err(|_| failed())?;
                Ok(HttpReply { status, body })
            })
        })
    }
}

/// Google client from `PLAY_INTEGRITY_SERVICE_ACCOUNT_FILE`, read at startup.
/// Unset, unreadable, or invalid: `None`, and `/v1/attest/play` answers 503.
fn play_client(client: &reqwest::Client) -> Option<GooglePlayIntegrity<ReqwestHttp>> {
    let Some(path) = std::env::var_os("PLAY_INTEGRITY_SERVICE_ACCOUNT_FILE") else {
        eprintln!("note: PLAY_INTEGRITY_SERVICE_ACCOUNT_FILE unset; /v1/attest/play disabled");
        return None;
    };
    let account = std::fs::read_to_string(&path)
        .map_err(|_| BrokerError::Misconfigured("play integrity service account unreadable"))
        .and_then(|json| ServiceAccount::from_json(&json));
    match account {
        Ok(account) => Some(GooglePlayIntegrity::new(
            account,
            ReqwestHttp {
                client: client.clone(),
            },
        )),
        Err(err) => {
            eprintln!("warning: {err}; /v1/attest/play disabled");
            None
        }
    }
}

type SharedBroker = Arc<tokio::sync::Mutex<Broker<Option<ReqwestSteamApi>, ReqwestDetritus>>>;

fn error_body(err: BrokerError) -> (u16, serde_json::Value) {
    (
        err.http_status(),
        serde_json::json!({"error": err.to_string()}),
    )
}

fn grant_body(result: Result<AttestedGrant, BrokerError>) -> (u16, serde_json::Value) {
    match result {
        Ok(grant) => (
            200,
            serde_json::json!({
                "token": grant.token,
                "exp": grant.exp,
                "scopes": grant.scopes.iter().map(|scope| scope.name()).collect::<Vec<_>>(),
                "protection": attestation_broker::protection_name(grant.protection),
            }),
        ),
        Err(err) => error_body(err),
    }
}

fn parse_report(value: &serde_json::Value) -> Option<trust_safety::SignedReport> {
    attestation_broker::parse_client_report(value)
}

/// Request body cap: bounded before parsing, rejected (not truncated).
const MAX_BODY_BYTES: usize = 1_048_576;
/// Per-connection deadline: slow readers cannot hold slots forever.
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// Concurrent connection bound: excess connections are dropped at accept.
const MAX_CONNECTIONS: usize = 64;
/// Upstream (Steam, Google, Detritus) connect deadline.
const UPSTREAM_CONNECT_TIMEOUT_SECS: u64 = 5;
/// Upstream whole-request deadline. A Play attestation makes at most two
/// sequential Google calls, which must fit the connection deadline.
const UPSTREAM_TIMEOUT_SECS: u64 = 10;

/// The address rate limits key on: the socket peer, or, only when the
/// operator configured `--client-ip-header`, the rightmost address in that
/// header (the one the trusted proxy appended). A missing or unparsable value
/// falls back to the peer.
fn client_ip(peer: IpAddr, forwarded: Option<&str>) -> IpAddr {
    forwarded
        .and_then(|value| value.rsplit(',').next())
        .map(str::trim)
        .and_then(|last| {
            last.parse::<IpAddr>()
                .ok()
                .or_else(|| last.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
        })
        .unwrap_or(peer)
}

async fn route(
    broker: SharedBroker,
    sink: Arc<tokio::sync::Mutex<FileReportSink>>,
    queue: Arc<tokio::sync::Mutex<FileDeletionQueue>>,
    caller: IpAddr,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, serde_json::Value) {
    if method == "GET" && path == "/health" {
        return (200, serde_json::json!({"ok": true}));
    }
    if method != "POST" {
        return (404, serde_json::json!({"error": "not found"}));
    }
    let body: serde_json::Value = match serde_json::from_str(body) {
        Ok(body) => body,
        Err(_) => {
            return error_body(BrokerError::Malformed("body is not json".into()));
        }
    };
    let now = platform_attestation::SystemClock;
    match path {
        "/v1/attest/steam" => {
            let Some(bytes) = body
                .get("ticket_hex")
                .and_then(|v| v.as_str())
                .and_then(hex_decode)
            else {
                return error_body(BrokerError::Malformed("ticket_hex required".into()));
            };
            let ticket = SteamTicket {
                bytes,
                identity_hint: body
                    .get("identity_hint")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            };
            let mut broker = broker.lock().await;
            grant_body(broker.attest_steam(&ticket, None, &now))
        }
        "/v1/attest/play/nonce" => {
            let issued = broker.lock().await.issue_play_nonce(caller, &now);
            match issued {
                Ok(nonce) => (200, serde_json::json!({"nonce": nonce})),
                Err(err) => error_body(err),
            }
        }
        "/v1/attest/play" => {
            let field = |name: &str| body.get(name).and_then(|v| v.as_str());
            let (Some(token), Some(claim), Some(nonce)) =
                (field("token"), field("claim"), field("nonce"))
            else {
                return error_body(BrokerError::Malformed(
                    "token, claim and nonce required".into(),
                ));
            };
            // Spend the nonce under the lock, then run the Google round trip
            // without it: a stalled decode must not hold every other route.
            let started = broker.lock().await.begin_play(nonce, &now);
            let decoded = match started.and_then(|pending| pending.decode(token, now.now_secs())) {
                Ok(decoded) => decoded,
                Err(err) => return error_body(err),
            };
            let finished = broker
                .lock()
                .await
                .finish_play(&decoded, nonce, claim, &now);
            grant_body(finished)
        }
        "/v1/reports" => {
            let (Some(token), Some(report)) = (
                body.get("token").and_then(|v| v.as_str()),
                body.get("report").and_then(parse_report),
            ) else {
                return error_body(BrokerError::Malformed("token and report required".into()));
            };
            let broker = broker.lock().await;
            let mut sink = sink.lock().await;
            match broker.submit_report(token, &report, &mut *sink, &now) {
                Ok(()) => (200, serde_json::json!({"ok": true})),
                Err(err) => error_body(err),
            }
        }
        "/v1/deletion" => {
            let Some(token) = body.get("token").and_then(|v| v.as_str()) else {
                return error_body(BrokerError::Malformed("token required".into()));
            };
            let request = diagnostics_policy::DeleteRequest {
                now: std::time::SystemTime::now(),
            };
            let broker = broker.lock().await;
            let mut queue = queue.lock().await;
            match broker.submit_deletion(token, &request, &mut *queue, &now) {
                Ok(deadline) => (200, serde_json::json!({"deadline": deadline})),
                Err(err) => error_body(err),
            }
        }
        "/v1/diagnostics/forward" => {
            let (Some(token), Some(payload_b64)) = (
                body.get("token").and_then(|v| v.as_str()),
                body.get("payload_b64").and_then(|v| v.as_str()),
            ) else {
                return error_body(BrokerError::Malformed(
                    "token and payload_b64 required".into(),
                ));
            };
            let payload = match base64_decode(payload_b64) {
                Some(payload) => payload,
                None => {
                    return error_body(BrokerError::Malformed("payload_b64 invalid".into()));
                }
            };
            let broker = broker.lock().await;
            match broker.forward_diagnostic(
                token,
                &payload,
                diagnostics_policy::Consent::Granted,
                diagnostics_policy::Purpose::GameplayLogs,
                &now,
            ) {
                Ok(()) => (200, serde_json::json!({"ok": true})),
                Err(err) => error_body(err),
            }
        }
        _ => (404, serde_json::json!({"error": "not found"})),
    }
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(input)
        .ok()
}

type Body = http_body_util::Full<bytes::Bytes>;

fn json_reply(
    status: u16,
    body: serde_json::Value,
) -> Result<hyper::Response<Body>, std::convert::Infallible> {
    let status =
        http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);
    Ok(hyper::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::new(bytes::Bytes::from(body.to_string())))
        .unwrap_or_else(|_| {
            hyper::Response::builder()
                .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::new(bytes::Bytes::from_static(
                    br#"{"error":"reply failed"}"#,
                )))
                .expect("static fallback reply builds")
        }))
}

async fn serve(
    req: hyper::Request<hyper::body::Incoming>,
    broker: SharedBroker,
    sink: Arc<tokio::sync::Mutex<FileReportSink>>,
    queue: Arc<tokio::sync::Mutex<FileDeletionQueue>>,
    peer: SocketAddr,
    client_ip_header: Option<Arc<str>>,
) -> Result<hyper::Response<Body>, std::convert::Infallible> {
    use http_body_util::BodyExt;
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    // The last header line: a proxy that adds its own line puts it last.
    let forwarded = client_ip_header
        .as_deref()
        .and_then(|name| req.headers().get_all(name).iter().next_back())
        .and_then(|value| value.to_str().ok());
    let caller = client_ip(peer.ip(), forwarded);
    let mut body_bytes = Vec::new();
    let mut body = req.into_body();
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return json_reply(400, serde_json::json!({"error": "body unreadable"}));
        };
        if let Some(data) = frame.data_ref() {
            // Reject oversized bodies instead of truncating them.
            if body_bytes.len() + data.len() > MAX_BODY_BYTES {
                return json_reply(413, serde_json::json!({"error": "body too large"}));
            }
            body_bytes.extend_from_slice(data);
        }
    }
    let text = String::from_utf8_lossy(&body_bytes);
    let (status, value) = route(broker, sink, queue, caller, &method, &path, &text).await;
    json_reply(status, value)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    let args = Args::parse();
    let (Ok(steam_api_key), Ok(token_hmac_key), Ok(detritus_bearer)) = (
        env_or_file("STEAM_WEB_API_KEY"),
        secret_bytes("TOKEN_HMAC_KEY"),
        env_or_file("DETRITUS_TOKEN"),
    ) else {
        eprintln!("refusing to serve: secret file unreadable");
        return ExitCode::from(2);
    };
    let secrets = EnvSecrets {
        steam_api_key,
        token_hmac_key,
        detritus_bearer,
    };
    let Some(play_adult_sources) = args
        .play_adult_age_sources
        .split(',')
        .map(|name| AgeRangeSource::parse(name.trim()))
        .collect::<Option<Vec<_>>>()
    else {
        eprintln!("refusing to serve: --play-adult-age-sources takes TIER_A..TIER_D");
        return ExitCode::from(2);
    };
    let client = match http_client::builder()
        .connect_timeout(std::time::Duration::from_secs(
            UPSTREAM_CONNECT_TIMEOUT_SECS,
        ))
        .timeout(std::time::Duration::from_secs(UPSTREAM_TIMEOUT_SECS))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            let err = http_client::error_chain(&err);
            eprintln!("refusing to serve: http client: {err}");
            return ExitCode::from(2);
        }
    };
    // A missing platform credential disables that attest route (503); the
    // other routes keep serving.
    let steam = secrets.steam_api_key().map(|api_key| ReqwestSteamApi {
        client: client.clone(),
        api_key,
        app_id: args.steam_app_id,
    });
    if steam.is_none() {
        eprintln!("note: STEAM_WEB_API_KEY(_FILE) unset; /v1/attest/steam disabled");
    }
    let play = play_client(&client);
    let detritus = ReqwestDetritus {
        client,
        url: args.detritus_url.clone(),
        bearer: match secrets.detritus_bearer() {
            Some(bearer) => bearer,
            None => {
                eprintln!("refusing to serve: DETRITUS_TOKEN(_FILE) unset");
                return ExitCode::from(2);
            }
        },
    };
    let mut config = BrokerConfig::new(
        args.steam_app_id,
        args.detritus_url,
        AttestationPolicy::new(args.play_package, args.play_age_hash_domain),
    );
    config.token_ttl_secs = args.token_ttl_secs;
    config.public_bind = args.bind.clone();
    config.play_adult_sources = play_adult_sources;
    let broker = match Broker::new(config, &secrets, steam, detritus) {
        Ok(broker) => Arc::new(tokio::sync::Mutex::new(match play {
            Some(play) => broker.with_play(play),
            None => broker,
        })),
        Err(err) => {
            eprintln!("refusing to serve: {err}");
            return ExitCode::from(2);
        }
    };
    let listener = match tokio::net::TcpListener::bind(&args.bind).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("bind {} failed: {err}", args.bind);
            return ExitCode::from(2);
        }
    };
    let data_dir = std::path::PathBuf::from(&args.data_dir);
    let sink = Arc::new(tokio::sync::Mutex::new(
        match FileReportSink::open(data_dir.join("reports.json")) {
            Ok(sink) => sink,
            Err(err) => {
                eprintln!("refusing to serve: report store unreadable: {err}");
                return ExitCode::from(2);
            }
        },
    ));
    if let Err(err) = sink.lock().await.prune() {
        eprintln!("refusing to serve: report retention cleanup failed: {err}");
        return ExitCode::from(2);
    }
    let janitor_sink = sink.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(err) = janitor_sink.lock().await.prune() {
                eprintln!("report retention cleanup failed: {err}");
            }
        }
    });
    let queue = Arc::new(tokio::sync::Mutex::new(
        match FileDeletionQueue::open(data_dir.join("deletions.json")) {
            Ok(queue) => queue,
            Err(err) => {
                eprintln!("refusing to serve: deletion queue unreadable: {err}");
                return ExitCode::from(2);
            }
        },
    ));
    let client_ip_header: Option<Arc<str>> = args.client_ip_header.as_deref().map(Arc::from);
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok((stream, peer)) = listener.accept().await else {
            continue;
        };
        // Shed load instead of queueing unbounded connections.
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let io = hyper_util::rt::TokioIo::new(stream);
        let service = hyper::service::service_fn({
            let (broker, sink, queue) = (broker.clone(), sink.clone(), queue.clone());
            let header = client_ip_header.clone();
            move |req| {
                let (broker, sink, queue) = (broker.clone(), sink.clone(), queue.clone());
                serve(req, broker, sink, queue, peer, header.clone())
            }
        });
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
                hyper::server::conn::http1::Builder::new()
                    .max_buf_size(16 * 1024)
                    .serve_connection(io, service),
            )
            .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, mpsc};

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("ip literal")
    }

    #[test]
    fn client_ip_is_the_peer_unless_the_trusted_header_names_one() {
        let peer = ip("10.0.0.1");
        assert_eq!(client_ip(peer, None), peer);
        // The proxy appends the address it saw; earlier entries are the
        // client's own claims.
        assert_eq!(
            client_ip(peer, Some("203.0.113.9, 198.51.100.7")),
            ip("198.51.100.7")
        );
        assert_eq!(
            client_ip(peer, Some("198.51.100.7:4711")),
            ip("198.51.100.7")
        );
        assert_eq!(client_ip(peer, Some("not-an-address")), peer);
    }

    /// Google decode that blocks until the test releases it.
    struct StalledGoogle {
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl attestation_broker::PlayIntegrityApi for StalledGoogle {
        fn decode_integrity_token(
            &self,
            _package: &str,
            _token: &str,
            _now_secs: u64,
        ) -> Result<String, BrokerError> {
            let _ = self.entered.send(());
            let _ = self.release.lock().expect("release lock").recv();
            Err(BrokerError::Upstream("stalled".into()))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stalled_google_decode_does_not_hold_the_broker() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel();
        let secrets = EnvSecrets {
            steam_api_key: None,
            token_hmac_key: Some(b"test-hmac-key".to_vec()),
            detritus_bearer: Some("bearer".to_string()),
        };
        let detritus = ReqwestDetritus {
            client: http_client::builder().build().unwrap(),
            url: "http://127.0.0.1:9".to_string(),
            bearer: "bearer".to_string(),
        };
        let config = BrokerConfig::new(
            1,
            "http://127.0.0.1:9".to_string(),
            AttestationPolicy::new("com.example.game", "example:play-age:v1"),
        );
        let broker = Broker::new(config, &secrets, None::<ReqwestSteamApi>, detritus)
            .expect("secrets complete")
            .with_play(StalledGoogle {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            });
        let broker: SharedBroker = Arc::new(tokio::sync::Mutex::new(broker));
        let dir = std::env::temp_dir().join(format!("broker_stall_{}", std::process::id()));
        let sink = Arc::new(tokio::sync::Mutex::new(
            FileReportSink::open(dir.join("reports.json")).expect("empty sink"),
        ));
        let queue = Arc::new(tokio::sync::Mutex::new(
            FileDeletionQueue::open(dir.join("deletions.json")).expect("empty queue"),
        ));
        let call = |path: &'static str, body: String| {
            let (broker, sink, queue) = (broker.clone(), sink.clone(), queue.clone());
            async move { route(broker, sink, queue, ip("198.51.100.7"), "POST", path, &body).await }
        };

        let (status, issued) = call("/v1/attest/play/nonce", "{}".to_string()).await;
        assert_eq!(status, 200);
        let body = serde_json::json!({"token": "t", "claim": "c", "nonce": issued["nonce"]});
        let attest = tokio::spawn(call("/v1/attest/play", body.to_string()));
        tokio::task::spawn_blocking(move || entered_rx.recv())
            .await
            .expect("join")
            .expect("decode entered");

        // Google is stalled mid-decode: every other route still gets the broker.
        let (status, _) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            call("/v1/attest/play/nonce", "{}".to_string()),
        )
        .await
        .expect("the broker lock is free while Google stalls");
        assert_eq!(status, 200);

        release_tx.send(()).expect("release decode");
        let (status, _) = attest.await.expect("attest task");
        assert_eq!(status, 502);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
