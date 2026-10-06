//! Checking the short-lived tokens Sol attaches when it proxies a request.
//!
//! Sol signs every token with Ed25519 for exactly one app (`aud`). Apps fetch
//! Sol's public keys from `/_sol/jwks` on first use and again whenever a token
//! names a key they have not seen, so containers can start in any order.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts},
};
use chrono_tz::Tz;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::{Deserialize, Serialize};

use crate::ApiError;

pub const ISSUER: &str = "sol";
/// Tokens live five minutes; Sol mints a fresh one per proxied request.
pub const TOKEN_TTL_SECS: i64 = 300;
const LEEWAY_SECS: u64 = 30;
const MIN_REFETCH: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// A signed-in person, via Sol's proxy.
    User,
    /// Sol's own background work, such as pulling the outbox.
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub iss: String,
    pub aud: String,
    /// User id for `user` tokens, `sol` for `system` tokens.
    pub sub: String,
    pub scope: Scope,
    /// IANA time zone of the user, so apps agree on what "today" is.
    pub tz: String,
    pub iat: i64,
    pub exp: i64,
}

impl Claims {
    pub fn tz(&self) -> Tz {
        self.tz.parse().unwrap_or(Tz::UTC)
    }

    /// The user's current calendar date.
    pub fn today(&self) -> chrono::NaiveDate {
        chrono::Utc::now().with_timezone(&self.tz()).date_naive()
    }
}

/// Verifies tokens addressed to one app.
#[derive(Clone)]
pub struct Verifier {
    inner: Arc<Inner>,
}

struct Inner {
    audience: String,
    jwks_url: Option<String>,
    http: reqwest::Client,
    keys: RwLock<HashMap<String, DecodingKey>>,
    last_fetch: tokio::sync::Mutex<Option<Instant>>,
}

impl Verifier {
    /// Fetches keys from Sol at `<sol_url>/_sol/jwks` when needed.
    pub fn remote(audience: &str, sol_url: &str) -> Self {
        Self::build(
            audience,
            Some(format!("{}/_sol/jwks", sol_url.trim_end_matches('/'))),
        )
    }

    /// Uses a fixed key set (tests, or Sol checking its own tokens).
    pub fn with_keys(audience: &str, set: &JwkSet) -> anyhow::Result<Self> {
        let verifier = Self::build(audience, None);
        verifier.store(set)?;
        Ok(verifier)
    }

    fn build(audience: &str, jwks_url: Option<String>) -> Self {
        Self {
            inner: Arc::new(Inner {
                audience: audience.to_owned(),
                jwks_url,
                http: reqwest::Client::new(),
                keys: RwLock::new(HashMap::new()),
                last_fetch: tokio::sync::Mutex::new(None),
            }),
        }
    }

    pub async fn verify(&self, token: &str) -> Result<Claims, ApiError> {
        let header = decode_header(token).map_err(|_| ApiError::unauthorized("malformed token"))?;
        // Only EdDSA, whatever the header claims; `Validation` enforces it again below.
        if header.alg != Algorithm::EdDSA {
            return Err(ApiError::unauthorized("unsupported token algorithm"));
        }
        let kid = header
            .kid
            .ok_or_else(|| ApiError::unauthorized("token has no key id"))?;
        let key = match self.key(&kid) {
            Some(key) => key,
            None => {
                self.refresh().await;
                self.key(&kid)
                    .ok_or_else(|| ApiError::unauthorized("unknown signing key"))?
            }
        };

        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_audience(&[&self.inner.audience]);
        validation.set_issuer(&[ISSUER]);
        validation.set_required_spec_claims(&["exp", "aud", "iss", "sub"]);
        validation.leeway = LEEWAY_SECS;
        decode::<Claims>(token, &key, &validation)
            .map(|data| data.claims)
            .map_err(|_| ApiError::unauthorized("invalid or expired token"))
    }

    fn key(&self, kid: &str) -> Option<DecodingKey> {
        self.inner.keys.read().ok()?.get(kid).cloned()
    }

    async fn refresh(&self) {
        let Some(url) = &self.inner.jwks_url else {
            return;
        };
        let mut last = self.inner.last_fetch.lock().await;
        if last.is_some_and(|at| at.elapsed() < MIN_REFETCH) {
            return;
        }
        *last = Some(Instant::now());
        let fetched = async {
            let set: JwkSet = self
                .inner
                .http
                .get(url)
                .timeout(Duration::from_secs(5))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            self.store(&set)
        };
        match fetched.await {
            Ok(()) => tracing::debug!(%url, "refreshed signing keys"),
            Err(err) => tracing::warn!(%url, error = %err, "could not fetch Sol's signing keys"),
        }
    }

    fn store(&self, set: &JwkSet) -> anyhow::Result<()> {
        let mut keys = HashMap::new();
        for jwk in &set.keys {
            if let Some(kid) = &jwk.common.key_id {
                keys.insert(kid.clone(), DecodingKey::from_jwk(jwk)?);
            }
        }
        *self
            .inner
            .keys
            .write()
            .map_err(|_| anyhow::anyhow!("key cache poisoned"))? = keys;
        Ok(())
    }
}

async fn bearer_claims(parts: &Parts, verifier: &Verifier) -> Result<Claims, ApiError> {
    let token = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
    verifier.verify(token).await
}

/// A signed-in person. Handlers that take it reject anything else.
pub struct User(pub Claims);

/// Sol itself. Only Sol's background tasks get these tokens; the proxy never does.
pub struct System(pub Claims);

impl<S> FromRequestParts<S> for User
where
    S: Send + Sync,
    Verifier: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let claims = bearer_claims(parts, &Verifier::from_ref(state)).await?;
        if claims.scope != Scope::User {
            return Err(ApiError::forbidden("this endpoint needs a user token"));
        }
        Ok(Self(claims))
    }
}

impl<S> FromRequestParts<S> for System
where
    S: Send + Sync,
    Verifier: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let claims = bearer_claims(parts, &Verifier::from_ref(state)).await?;
        if claims.scope != Scope::System {
            return Err(ApiError::forbidden("this endpoint is for Sol only"));
        }
        Ok(Self(claims))
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{SigningKey, pkcs8::EncodePrivateKey};
    use jsonwebtoken::{EncodingKey, Header, encode};

    use super::*;

    fn keypair() -> (EncodingKey, JwkSet) {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let der = signing.to_pkcs8_der().unwrap();
        let x = URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes());
        let set = serde_json::from_value(serde_json::json!({
            "keys": [{ "kty": "OKP", "crv": "Ed25519", "x": x, "kid": "k1", "alg": "EdDSA", "use": "sig" }]
        }))
        .unwrap();
        (EncodingKey::from_ed_der(der.as_bytes()), set)
    }

    fn token(key: &EncodingKey, aud: &str, scope: Scope, exp_in: i64) -> String {
        let now = chrono::Utc::now().timestamp();
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some("k1".into());
        let claims = Claims {
            iss: ISSUER.into(),
            aud: aud.into(),
            sub: "u1".into(),
            scope,
            tz: "Europe/Copenhagen".into(),
            iat: now,
            exp: now + exp_in,
        };
        encode(&header, &claims, key).unwrap()
    }

    #[tokio::test]
    async fn accepts_only_its_own_audience_and_live_tokens() {
        let (key, set) = keypair();
        let terra = Verifier::with_keys("terra", &set).unwrap();

        let ok = terra
            .verify(&token(&key, "terra", Scope::User, 60))
            .await
            .unwrap();
        assert_eq!(ok.sub, "u1");
        assert_eq!(ok.tz(), Tz::Europe__Copenhagen);

        assert!(
            terra
                .verify(&token(&key, "neptune", Scope::User, 60))
                .await
                .is_err()
        );
        assert!(
            terra
                .verify(&token(&key, "terra", Scope::User, -120))
                .await
                .is_err()
        );
        assert!(terra.verify("not.a.token").await.is_err());
    }

    #[tokio::test]
    async fn rejects_unsigned_tokens() {
        let (_, set) = keypair();
        let terra = Verifier::with_keys("terra", &set).unwrap();
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","kid":"k1"}"#);
        let body = URL_SAFE_NO_PAD.encode(r#"{"iss":"sol","aud":"terra","sub":"u1","scope":"user","tz":"UTC","iat":0,"exp":9999999999}"#);
        assert!(terra.verify(&format!("{header}.{body}.")).await.is_err());
    }
}
