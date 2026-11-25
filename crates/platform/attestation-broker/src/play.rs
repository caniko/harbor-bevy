//! Google Play Integrity decode client.
//!
//! Standard Integrity tokens are decrypted only by Google: the broker posts
//! the opaque token to `decodeIntegrityToken` under an OAuth access token
//! minted from a service account (RS256 JWT-bearer grant). HTTP is injected
//! through [`HttpPost`] so tests fake both the OAuth exchange and the decode
//! call; the binary supplies a reqwest implementation.
//!
//! Errors carry fixed strings only: never tokens, keys, or upstream bodies.

use std::fmt;
use std::sync::{Mutex, MutexGuard};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

use crate::BrokerError;

/// OAuth scope for `decodeIntegrityToken`.
pub const PLAY_INTEGRITY_SCOPE: &str = "https://www.googleapis.com/auth/playintegrity";

/// Play Integrity API origin.
pub const PLAY_INTEGRITY_API: &str = "https://playintegrity.googleapis.com";

/// Lifetime requested for the service-account JWT (Google's maximum).
const JWT_TTL_SECS: u64 = 3600;

/// Access tokens are refreshed this long before Google's stated expiry.
const ACCESS_TOKEN_MARGIN_SECS: u64 = 60;

/// Decodes a Standard Integrity token into Google's verdict JSON (the full
/// `decodeIntegrityToken` response body).
/// Takes `&self`: the broker shares one client across concurrent requests.
pub trait PlayIntegrityApi {
    fn decode_integrity_token(
        &self,
        package: &str,
        token: &str,
        now_secs: u64,
    ) -> Result<String, BrokerError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpReply {
    pub status: u16,
    pub body: String,
}

/// One HTTPS POST. Implementations must not put the body or bearer in errors.
pub trait HttpPost {
    fn post(
        &self,
        url: &str,
        content_type: &str,
        bearer: Option<&str>,
        body: String,
    ) -> Result<HttpReply, BrokerError>;
}

type Signer = Box<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>;

/// Service-account identity plus its RS256 signer. The private key never
/// leaves the signer; [`fmt::Debug`] shows the account email only.
pub struct ServiceAccount {
    client_email: String,
    key_id: String,
    token_uri: String,
    signer: Signer,
}

impl fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAccount")
            .field("client_email", &self.client_email)
            .finish_non_exhaustive()
    }
}

impl ServiceAccount {
    /// Parse a Google service-account key file (JSON with `client_email`,
    /// `private_key_id`, `private_key` as PKCS#8 PEM, and `token_uri`).
    pub fn from_json(json: &str) -> Result<Self, BrokerError> {
        let bad = || BrokerError::Misconfigured("play integrity service account");
        let value: serde_json::Value = serde_json::from_str(json).map_err(|_| bad())?;
        let field = |name: &str| {
            value
                .get(name)
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .ok_or_else(bad)
        };
        let token_uri = field("token_uri")?;
        if !token_uri.starts_with("https://") {
            return Err(bad());
        }
        let pem = field("private_key")?;
        let der_b64: String = pem
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        let der = STANDARD.decode(der_b64.trim()).map_err(|_| bad())?;
        let key = ring::signature::RsaKeyPair::from_pkcs8(&der).map_err(|_| bad())?;
        let rng = ring::rand::SystemRandom::new();
        Ok(Self::with_signer(
            field("client_email")?,
            field("private_key_id")?,
            token_uri,
            Box::new(move |message| {
                let mut sig = vec![0; key.public().modulus_len()];
                key.sign(&ring::signature::RSA_PKCS1_SHA256, &rng, message, &mut sig)
                    .ok()?;
                Some(sig)
            }),
        ))
    }

    /// Account with an injected signer (tests; no RSA key in the tree).
    pub fn with_signer(
        client_email: String,
        key_id: String,
        token_uri: String,
        signer: Signer,
    ) -> Self {
        Self {
            client_email,
            key_id,
            token_uri,
            signer,
        }
    }

    /// RS256 JWT-bearer assertion for the Play Integrity scope.
    fn jwt(&self, now_secs: u64) -> Result<String, BrokerError> {
        let header = serde_json::json!({"alg": "RS256", "typ": "JWT", "kid": self.key_id});
        let claims = serde_json::json!({
            "iss": self.client_email,
            "scope": PLAY_INTEGRITY_SCOPE,
            "aud": self.token_uri,
            "iat": now_secs,
            "exp": now_secs.saturating_add(JWT_TTL_SECS),
        });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let sig = (self.signer)(signing_input.as_bytes())
            .ok_or(BrokerError::Misconfigured("play integrity signer"))?;
        Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig)))
    }
}

struct AccessToken {
    token: String,
    refresh_at: u64,
}

/// Real Google client: OAuth exchange (cached) + `decodeIntegrityToken`.
pub struct GooglePlayIntegrity<H> {
    account: ServiceAccount,
    http: H,
    /// Held across a refresh, so concurrent decodes share one OAuth exchange.
    access: Mutex<Option<AccessToken>>,
}

// ponytail: manual Debug so the cached access token never reaches logs.
impl<H> fmt::Debug for GooglePlayIntegrity<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GooglePlayIntegrity")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl<H: HttpPost> GooglePlayIntegrity<H> {
    pub fn new(account: ServiceAccount, http: H) -> Self {
        Self {
            account,
            http,
            access: Mutex::new(None),
        }
    }

    /// A poisoned lock only means a panicked refresh; the cache is still a
    /// plain value, so recover it.
    fn access(&self) -> MutexGuard<'_, Option<AccessToken>> {
        self.access
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn access_token(&self, now_secs: u64) -> Result<String, BrokerError> {
        let mut cached = self.access();
        if let Some(access) = cached.as_ref()
            && access.refresh_at > now_secs
        {
            return Ok(access.token.clone());
        }
        let body = format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={}",
            self.account.jwt(now_secs)?
        );
        let failed = || BrokerError::Upstream("play integrity oauth failed".into());
        let reply = self.http.post(
            &self.account.token_uri,
            "application/x-www-form-urlencoded",
            None,
            body,
        )?;
        if reply.status != 200 {
            return Err(failed());
        }
        let value: serde_json::Value = serde_json::from_str(&reply.body).map_err(|_| failed())?;
        let token = value
            .get("access_token")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .ok_or_else(failed)?
            .to_string();
        let expires_in = value
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .ok_or_else(failed)?;
        *cached = Some(AccessToken {
            token: token.clone(),
            refresh_at: now_secs
                .saturating_add(expires_in)
                .saturating_sub(ACCESS_TOKEN_MARGIN_SECS),
        });
        Ok(token)
    }
}

impl<H: HttpPost> PlayIntegrityApi for GooglePlayIntegrity<H> {
    fn decode_integrity_token(
        &self,
        package: &str,
        token: &str,
        now_secs: u64,
    ) -> Result<String, BrokerError> {
        let access = self.access_token(now_secs)?;
        let url = format!("{PLAY_INTEGRITY_API}/v1/{package}:decodeIntegrityToken");
        let body = serde_json::json!({"integrity_token": token}).to_string();
        let reply = self
            .http
            .post(&url, "application/json", Some(&access), body)?;
        match reply.status {
            200 => Ok(reply.body),
            // Google rejects garbage, foreign, or expired tokens with 400.
            400 => Err(BrokerError::Malformed("integrity token rejected".into())),
            401 | 403 => {
                *self.access() = None;
                Err(BrokerError::Upstream("play integrity auth failed".into()))
            }
            _ => Err(BrokerError::Upstream("play integrity decode failed".into())),
        }
    }
}
