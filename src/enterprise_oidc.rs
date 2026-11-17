//! Minimal OpenID Connect client for one Okta application, used by both the relay
//! (Authorization Code + PKCE on behalf of the browser) and the connector (Device
//! Authorization Grant). Both flows use the same issuer and client, so the ID
//! token `sub` names the same Okta user on both ends. Token verification is
//! delegated to the maintained `jwt-simple` library; nothing here is logged.
use crate::enterprise_config::Policy;
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jwt_simple::prelude::{
    Audiences, Duration as TokenDuration, RS256PublicKey, RSAPublicKeyLike, Token,
    VerificationOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE: usize = 256 * 1024;
const MAX_TOKEN: usize = 16 * 1024;
const CLOCK_TOLERANCE_SECONDS: u64 = 60;
pub const SCOPES: &str = "openid profile offline_access";

/// Stable corporate identity: validated issuer plus immutable subject. Display
/// names and email addresses are never compared.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Principal {
    pub iss: String,
    pub sub: String,
}
#[derive(Clone, Debug)]
pub struct Identity {
    pub principal: Principal,
    /// Untrusted display text for the signed-in user only.
    pub name: String,
    /// When Okta issued the verified token (Unix seconds).
    pub issued_at: u64,
}
pub struct Tokens {
    pub identity: Identity,
    pub id_token: String,
    pub refresh_token: Option<String>,
}
/// `Rejected` is a definitive answer from Okta (revoked, disabled, denied);
/// `Unavailable` is everything else. Both deny access; only the first ends a session.
#[derive(Debug)]
pub enum Failure {
    Rejected,
    Unavailable,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Rejected => "Okta rejected the authorization",
            Self::Unavailable => "Okta could not be reached or returned an invalid response",
        })
    }
}
impl std::error::Error for Failure {}

#[derive(Serialize, Deserialize)]
struct Extra {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    azp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    acr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    amr: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preferred_username: Option<String>,
}
struct Endpoints {
    authorization: String,
    token: String,
    jwks: String,
    device: Option<String>,
}
#[derive(Default)]
struct Keys {
    keys: HashMap<String, (Vec<u8>, Vec<u8>)>,
    fetched: Option<Instant>,
}
pub struct DevicePrompt {
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub user_code: String,
    pub expires_in: u64,
}
pub struct Oidc {
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
    acr_values: Option<String>,
    required_amr: Vec<String>,
    http: reqwest::Client,
    endpoints: Endpoints,
    keys: tokio::sync::Mutex<Keys>,
}
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
fn display(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(128)
        .collect::<String>()
        .trim()
        .to_owned()
}

impl Oidc {
    /// Fails when the provider metadata cannot be fetched and verified: enabled
    /// enterprise mode never starts without a working identity provider.
    pub async fn discover(policy: &Policy, client_secret: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .connect_timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("herdr-webapp/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let url = format!("{}/.well-known/openid-configuration", policy.issuer);
        let metadata = get_json(&http, &url)
            .await
            .context("Okta discovery failed; check the issuer and network access")?;
        ensure!(
            metadata["issuer"] == policy.issuer.as_str(),
            "Okta discovery document names a different issuer"
        );
        let origin = url::Url::parse(&policy.issuer)?.origin();
        let endpoint = |name: &str| -> Result<String> {
            let value = metadata[name]
                .as_str()
                .with_context(|| format!("Okta discovery lacks {name}"))?;
            ensure!(
                value.len() <= 1024 && url::Url::parse(value)?.origin() == origin,
                "Okta {name} is not on the issuer origin"
            );
            Ok(value.to_owned())
        };
        let supports = |name: &str, wanted: &str| {
            metadata[name]
                .as_array()
                .is_none_or(|values| values.iter().any(|v| v == wanted))
        };
        ensure!(
            supports("id_token_signing_alg_values_supported", "RS256")
                && supports("code_challenge_methods_supported", "S256"),
            "Okta issuer does not support RS256 ID tokens with PKCE S256"
        );
        Ok(Self {
            issuer: policy.issuer.clone(),
            client_id: policy.client_id.clone(),
            client_secret,
            acr_values: policy.acr_values.clone(),
            required_amr: policy.required_amr.clone(),
            endpoints: Endpoints {
                authorization: endpoint("authorization_endpoint")?,
                token: endpoint("token_endpoint")?,
                jwks: endpoint("jwks_uri")?,
                device: metadata["device_authorization_endpoint"]
                    .as_str()
                    .map(|_| endpoint("device_authorization_endpoint"))
                    .transpose()?,
            },
            http,
            keys: Default::default(),
        })
    }

    pub fn authorization_url(
        &self,
        redirect_uri: &str,
        state: &str,
        nonce: &str,
        verifier: &str,
    ) -> Result<String> {
        let mut url = url::Url::parse(&self.endpoints.authorization)?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", &self.client_id)
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("scope", SCOPES)
                .append_pair("state", state)
                .append_pair("nonce", nonce)
                .append_pair("code_challenge", &pkce_challenge(verifier))
                .append_pair("code_challenge_method", "S256");
            if let Some(acr) = &self.acr_values {
                query.append_pair("acr_values", acr);
            }
        }
        Ok(url.into())
    }

    async fn token_request(&self, form: &[(&str, &str)]) -> Result<Value, Failure> {
        let mut form = form.to_vec();
        let mut request = self.http.post(&self.endpoints.token);
        // A confidential client secret never leaves this process except to Okta.
        match &self.client_secret {
            Some(secret) => request = request.basic_auth(&self.client_id, Some(secret)),
            None => form.push(("client_id", &self.client_id)),
        }
        let response = request
            .header("accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(|_| Failure::Unavailable)?;
        let status = response.status();
        let body = bounded_json(response)
            .await
            .map_err(|_| Failure::Unavailable)?;
        if status.is_success() {
            return Ok(body);
        }
        // Only an OAuth error object is a definitive refusal; proxies and outages are not.
        if status.is_client_error()
            && status != reqwest::StatusCode::TOO_MANY_REQUESTS
            && body["error"].is_string()
        {
            return Err(Failure::Rejected);
        }
        Err(Failure::Unavailable)
    }

    async fn tokens(&self, body: Value, nonce: Option<&str>) -> Result<Tokens, Failure> {
        let id_token = body["id_token"].as_str().ok_or(Failure::Unavailable)?;
        let identity = self
            .verify_id_token(id_token, nonce)
            .await
            .map_err(|_| Failure::Rejected)?;
        Ok(Tokens {
            identity,
            id_token: id_token.to_owned(),
            refresh_token: body["refresh_token"]
                .as_str()
                .filter(|t| !t.is_empty() && t.len() <= 4096)
                .map(str::to_owned),
        })
    }

    pub async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
        nonce: &str,
    ) -> Result<Tokens, Failure> {
        let body = self
            .token_request(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("code_verifier", verifier),
            ])
            .await?;
        self.tokens(body, Some(nonce)).await
    }

    /// The periodic entitlement check: Okta revokes refresh tokens when a user is
    /// deactivated or their sessions and tokens are revoked.
    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens, Failure> {
        let body = self
            .token_request(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("scope", SCOPES),
            ])
            .await?;
        self.tokens(body, None).await
    }

    pub async fn device_login(&self, announce: impl FnOnce(&DevicePrompt)) -> Result<Tokens> {
        let endpoint = self.endpoints.device.as_deref().context(
            "the Okta application does not offer the Device Authorization grant; enable it for this client",
        )?;
        let response = self
            .http
            .post(endpoint)
            .header("accept", "application/json")
            .form(&[("client_id", self.client_id.as_str()), ("scope", SCOPES)])
            .send()
            .await
            .context("Okta device authorization request failed")?;
        ensure!(
            response.status().is_success(),
            "Okta refused device authorization; enable the Device Authorization grant for this client"
        );
        let body = bounded_json(response).await?;
        let text = |name: &str| -> Result<String> {
            let value = body[name]
                .as_str()
                .with_context(|| format!("Okta device response lacks {name}"))?;
            ensure!(
                !value.is_empty() && value.len() <= 2048 && !value.chars().any(char::is_control),
                "invalid Okta device response"
            );
            Ok(value.to_owned())
        };
        let device_code = text("device_code")?;
        let expires_in = body["expires_in"].as_u64().unwrap_or(600).clamp(30, 1800);
        let mut interval = body["interval"].as_u64().unwrap_or(5).clamp(1, 30);
        announce(&DevicePrompt {
            verification_uri: text("verification_uri")?,
            verification_uri_complete: body["verification_uri_complete"]
                .as_str()
                .and_then(|_| text("verification_uri_complete").ok()),
            user_code: text("user_code")?,
            expires_in,
        });
        let deadline = Instant::now() + Duration::from_secs(expires_in);
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            ensure!(
                Instant::now() < deadline,
                "Okta sign-in was not completed in time; start the connector again"
            );
            let mut form = vec![
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code.as_str()),
            ];
            let mut request = self.http.post(&self.endpoints.token);
            match &self.client_secret {
                Some(secret) => request = request.basic_auth(&self.client_id, Some(secret)),
                None => form.push(("client_id", &self.client_id)),
            }
            let Ok(response) = request
                .header("accept", "application/json")
                .form(&form)
                .send()
                .await
            else {
                continue;
            };
            let success = response.status().is_success();
            let Ok(body) = bounded_json(response).await else {
                continue;
            };
            if success {
                return self.tokens(body, None).await.map_err(|_| {
                    anyhow::anyhow!("Okta returned an ID token that failed validation")
                });
            }
            match body["error"].as_str() {
                Some("authorization_pending") => {}
                Some("slow_down") => interval = (interval + 5).min(60),
                Some("access_denied") => bail!("Okta sign-in was denied"),
                Some("expired_token") => bail!("Okta sign-in expired; start the connector again"),
                _ => bail!("Okta device sign-in failed"),
            }
        }
    }

    async fn key(&self, kid: &str) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut keys = self.keys.lock().await;
        if let Some(key) = keys.keys.get(kid) {
            return Ok(key.clone());
        }
        // Unknown key IDs may follow rotation, but must not become a request amplifier.
        ensure!(
            keys.fetched
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(60)),
            "unknown ID token signing key"
        );
        keys.fetched = Some(Instant::now());
        let document = get_json(&self.http, &self.endpoints.jwks).await?;
        let mut fresh = HashMap::new();
        for key in document["keys"]
            .as_array()
            .context("invalid JWKS")?
            .iter()
            .take(32)
        {
            if key["kty"] != "RSA" || key["use"].as_str().is_some_and(|u| u != "sig") {
                continue;
            }
            if let (Some(id), Some(n), Some(e)) =
                (key["kid"].as_str(), key["n"].as_str(), key["e"].as_str())
                && let (Ok(n), Ok(e)) = (URL_SAFE_NO_PAD.decode(n), URL_SAFE_NO_PAD.decode(e))
                && n.len() >= 256
            {
                fresh.insert(id.to_owned(), (n, e));
            }
        }
        keys.keys = fresh;
        keys.keys
            .get(kid)
            .cloned()
            .context("unknown ID token signing key")
    }

    /// Signature, issuer, audience, expiry, issue time, nonce and the configured
    /// authentication-context requirements. Any failure is a refusal.
    pub async fn verify_id_token(&self, token: &str, nonce: Option<&str>) -> Result<Identity> {
        ensure!(token.len() <= MAX_TOKEN, "ID token too large");
        let metadata =
            Token::decode_metadata(token).map_err(|_| anyhow::anyhow!("malformed ID token"))?;
        ensure!(
            metadata.algorithm() == "RS256",
            "unsupported ID token algorithm"
        );
        let kid = metadata.key_id().context("ID token lacks a key ID")?;
        ensure!(kid.len() <= 256, "invalid ID token key ID");
        let (n, e) = self.key(kid).await?;
        let key = RS256PublicKey::from_components(&n, &e)
            .map_err(|_| anyhow::anyhow!("invalid Okta signing key"))?;
        let options = VerificationOptions {
            allowed_issuers: Some(HashSet::from([self.issuer.clone()])),
            allowed_audiences: Some(HashSet::from([self.client_id.clone()])),
            required_nonce: nonce.map(str::to_owned),
            time_tolerance: Some(TokenDuration::from_secs(CLOCK_TOLERANCE_SECONDS)),
            max_token_length: Some(MAX_TOKEN),
            ..Default::default()
        };
        let claims = key
            .verify_token::<Extra>(token, Some(options))
            .map_err(|_| anyhow::anyhow!("ID token rejected"))?;
        // The library tolerates absent optional claims; OIDC requires all of these.
        ensure!(
            claims.issuer.as_deref() == Some(self.issuer.as_str())
                && claims.expires_at.is_some()
                && claims.issued_at.is_some(),
            "ID token lacks required claims"
        );
        match &claims.audiences {
            Some(Audiences::AsString(audience)) => {
                ensure!(*audience == self.client_id, "wrong audience")
            }
            Some(Audiences::AsSet(set)) => ensure!(
                set.contains(&self.client_id)
                    && (set.len() == 1 || claims.custom.azp.as_deref() == Some(&self.client_id)),
                "wrong audience"
            ),
            None => bail!("ID token lacks an audience"),
        }
        ensure!(
            claims
                .custom
                .azp
                .as_deref()
                .is_none_or(|azp| azp == self.client_id),
            "wrong authorized party"
        );
        let issued_at = claims.issued_at.map(|t| t.as_secs()).unwrap_or(0);
        let sub = claims.subject.context("ID token lacks a subject")?;
        ensure!(
            (1..=255).contains(&sub.len()) && !sub.chars().any(char::is_control),
            "invalid ID token subject"
        );
        // MFA is decided by Okta policy; these prove what Okta says it enforced.
        if let Some(required) = &self.acr_values {
            ensure!(
                claims
                    .custom
                    .acr
                    .as_deref()
                    .is_some_and(|acr| required.split(' ').any(|v| v == acr)),
                "ID token lacks the required authentication context"
            );
        }
        let amr = claims.custom.amr.unwrap_or_default();
        ensure!(
            self.required_amr.iter().all(|method| amr.contains(method)),
            "ID token lacks a required authentication method"
        );
        let name = claims
            .custom
            .name
            .or(claims.custom.preferred_username)
            .map(|n| display(&n))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Signed in".into());
        Ok(Identity {
            principal: Principal {
                iss: self.issuer.clone(),
                sub,
            },
            name,
            issued_at,
        })
    }
}

async fn bounded_json(mut response: reqwest::Response) -> Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= MAX_RESPONSE,
            "response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}
async fn get_json(http: &reqwest::Client, url: &str) -> Result<Value> {
    let response = http
        .get(url)
        .header("accept", "application/json")
        .send()
        .await?;
    ensure!(response.status().is_success(), "unexpected HTTP status");
    bounded_json(response).await
}

/// In-process identity provider for tests: real HTTP, real RS256 signatures.
#[cfg(test)]
pub mod mock {
    use super::*;
    use axum::{
        Form, Json, Router,
        extract::{Query, State},
        routing::{get, post},
    };
    use jwt_simple::prelude::{Claims, RS256KeyPair, RSAKeyPairLike};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub struct Inner {
        /// Authorization codes and device codes: value -> (subject, nonce).
        pub codes: HashMap<String, (String, Option<String>)>,
        /// Refresh tokens -> subject; removing one models Okta revocation.
        pub refresh: HashMap<String, String>,
        pub pending_devices: HashSet<String>,
        pub outage: bool,
        pub amr: Vec<String>,
        pub next: u64,
    }
    pub struct Idp {
        pub issuer: String,
        pub client_id: String,
        key: RS256KeyPair,
        other: RS256KeyPair,
        pub inner: Mutex<Inner>,
    }
    #[derive(Default)]
    pub struct Forge<'a> {
        pub issuer: Option<&'a str>,
        pub audience: Option<&'a str>,
        pub nonce: Option<&'a str>,
        pub expired: bool,
        pub wrong_key: bool,
    }
    impl Idp {
        pub async fn start() -> Arc<Self> {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let issuer = format!("http://{}", listener.local_addr().unwrap());
            let idp = Arc::new(Self {
                issuer,
                client_id: "0oatestclient".into(),
                key: RS256KeyPair::generate(2048)
                    .unwrap()
                    .with_key_id("test-key"),
                other: RS256KeyPair::generate(2048)
                    .unwrap()
                    .with_key_id("test-key"),
                inner: Mutex::new(Inner {
                    amr: vec!["pwd".into(), "mfa".into()],
                    ..Default::default()
                }),
            });
            let app = Router::new()
                .route("/.well-known/openid-configuration", get(discovery))
                .route("/keys", get(keys))
                .route("/authorize", get(authorize))
                .route("/token", post(token))
                .route("/device", post(device))
                .with_state(idp.clone());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            idp
        }
        pub fn policy(&self, relay_url: &str) -> Policy {
            Policy {
                source: "test".into(),
                relay_url: relay_url.into(),
                issuer: self.issuer.clone(),
                client_id: self.client_id.clone(),
                client_secret_file: None,
                revocations_file: None,
                acr_values: None,
                required_amr: vec![],
                grant_lifetime_seconds: 300,
                recheck_seconds: 300,
                session_max_seconds: 3600,
            }
        }
        fn fresh(&self, prefix: &str) -> String {
            let mut inner = self.inner.lock().unwrap();
            inner.next += 1;
            format!(
                "{prefix}-{}-{}",
                inner.next,
                crate::relay_crypto::random_token(8)
            )
        }
        pub fn token(&self, sub: &str, forge: Forge) -> String {
            #[derive(Serialize, Deserialize)]
            struct Custom {
                amr: Vec<String>,
                name: String,
            }
            let amr = self.inner.lock().unwrap().amr.clone();
            let mut claims = Claims::with_custom_claims(
                Custom {
                    amr,
                    name: format!("User {sub}"),
                },
                TokenDuration::from_secs(3600),
            )
            .with_issuer(forge.issuer.unwrap_or(&self.issuer))
            .with_audience(forge.audience.unwrap_or(&self.client_id))
            .with_subject(sub);
            if let Some(nonce) = forge.nonce {
                claims = claims.with_nonce(nonce);
            }
            if forge.expired {
                let past =
                    jwt_simple::prelude::Clock::now_since_epoch() - TokenDuration::from_secs(7200);
                claims.issued_at = Some(past);
                claims.invalid_before = Some(past);
                claims.expires_at = Some(past + TokenDuration::from_secs(600));
            }
            if forge.wrong_key {
                &self.other
            } else {
                &self.key
            }
            .sign(claims)
            .unwrap()
        }
        /// What a completed interactive Okta login would hand back to the redirect URI.
        pub fn approve_code(&self, sub: &str, nonce: &str) -> String {
            let code = self.fresh("code");
            self.inner
                .lock()
                .unwrap()
                .codes
                .insert(code.clone(), (sub.into(), Some(nonce.into())));
            code
        }
        pub fn approve_devices(&self, sub: &str) {
            let mut inner = self.inner.lock().unwrap();
            for code in std::mem::take(&mut inner.pending_devices) {
                inner.codes.insert(code, (sub.into(), None));
            }
        }
        pub fn revoke(&self, sub: &str) {
            self.inner.lock().unwrap().refresh.retain(|_, s| s != sub);
        }
        pub fn issue_refresh(&self, sub: &str) -> String {
            let token = self.fresh("refresh");
            self.inner
                .lock()
                .unwrap()
                .refresh
                .insert(token.clone(), sub.into());
            token
        }
    }
    async fn discovery(State(idp): State<Arc<Idp>>) -> Json<Value> {
        let i = &idp.issuer;
        Json(
            json!({"issuer":i,"authorization_endpoint":format!("{i}/authorize"),
            "token_endpoint":format!("{i}/token"),"jwks_uri":format!("{i}/keys"),
            "device_authorization_endpoint":format!("{i}/device"),
            "id_token_signing_alg_values_supported":["RS256"],
            "code_challenge_methods_supported":["S256"]}),
        )
    }
    async fn keys(State(idp): State<Arc<Idp>>) -> Json<Value> {
        let parts = idp.key.public_key().to_components();
        Json(
            json!({"keys":[{"kty":"RSA","use":"sig","alg":"RS256","kid":"test-key",
            "n":URL_SAFE_NO_PAD.encode(parts.n),"e":URL_SAFE_NO_PAD.encode(parts.e)}]}),
        )
    }
    /// Stands in for the interactive Okta page: `login_hint` selects the test user.
    async fn authorize(
        State(idp): State<Arc<Idp>>,
        Query(q): Query<HashMap<String, String>>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        if q.get("client_id") != Some(&idp.client_id)
            || q.get("code_challenge_method").map(String::as_str) != Some("S256")
        {
            return axum::http::StatusCode::BAD_REQUEST.into_response();
        }
        let sub = q
            .get("login_hint")
            .cloned()
            .unwrap_or_else(|| "user-a".into());
        let code = idp.approve_code(&sub, &q["nonce"]);
        let mut url = url::Url::parse(&q["redirect_uri"]).unwrap();
        url.query_pairs_mut()
            .append_pair("code", &code)
            .append_pair("state", &q["state"]);
        axum::response::Redirect::to(url.as_str()).into_response()
    }
    async fn device(State(idp): State<Arc<Idp>>) -> Json<Value> {
        let code = idp.fresh("device");
        idp.inner
            .lock()
            .unwrap()
            .pending_devices
            .insert(code.clone());
        Json(
            json!({"device_code":code,"user_code":"TEST-CODE","verification_uri":format!("{}/activate",idp.issuer),
            "verification_uri_complete":format!("{}/activate?user_code=TEST-CODE",idp.issuer),"interval":1,"expires_in":60}),
        )
    }
    async fn token(
        State(idp): State<Arc<Idp>>,
        Form(form): Form<HashMap<String, String>>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        use axum::http::StatusCode;
        let fail = |error: &str| (StatusCode::BAD_REQUEST, Json(json!({"error":error})));
        if idp.inner.lock().unwrap().outage {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
        }
        if form.get("client_id") != Some(&idp.client_id) {
            return fail("invalid_client");
        }
        let (sub, nonce) = match form.get("grant_type").map(String::as_str) {
            Some("authorization_code") => {
                if form.get("code_verifier").is_none_or(|v| v.len() < 43) {
                    return fail("invalid_grant");
                }
                match idp.inner.lock().unwrap().codes.remove(&form["code"]) {
                    Some(found) => found,
                    None => return fail("invalid_grant"),
                }
            }
            Some("urn:ietf:params:oauth:grant-type:device_code") => {
                let mut inner = idp.inner.lock().unwrap();
                let code = &form["device_code"];
                if inner.pending_devices.contains(code) {
                    return fail("authorization_pending");
                }
                match inner.codes.remove(code) {
                    Some(found) => found,
                    None => return fail("expired_token"),
                }
            }
            Some("refresh_token") => {
                match idp
                    .inner
                    .lock()
                    .unwrap()
                    .refresh
                    .get(&form["refresh_token"])
                {
                    Some(sub) => (sub.clone(), None),
                    None => return fail("invalid_grant"),
                }
            }
            _ => return fail("unsupported_grant_type"),
        };
        let mut body = json!({"token_type":"Bearer","id_token":idp.token(&sub, Forge{nonce:nonce.as_deref(),..Default::default()})});
        if form["grant_type"] != "refresh_token" {
            body["refresh_token"] = json!(idp.issue_refresh(&sub));
        }
        (StatusCode::OK, Json(body))
    }
}

#[cfg(test)]
mod tests {
    use super::{mock::*, *};
    #[tokio::test]
    async fn id_tokens_are_validated_against_issuer_audience_signature_expiry_and_nonce() {
        let idp = Idp::start().await;
        let policy = idp.policy("https://relay.example");
        let oidc = Oidc::discover(&policy, None).await.unwrap();
        let good = idp.token(
            "user-a",
            Forge {
                nonce: Some("n1"),
                ..Default::default()
            },
        );
        let identity = oidc.verify_id_token(&good, Some("n1")).await.unwrap();
        assert_eq!(
            identity.principal,
            Principal {
                iss: idp.issuer.clone(),
                sub: "user-a".into()
            }
        );
        assert_eq!(identity.name, "User user-a");
        assert!(oidc.verify_id_token(&good, Some("other")).await.is_err());
        assert!(
            oidc.verify_id_token(&idp.token("user-a", Forge::default()), Some("n1"))
                .await
                .is_err()
        );
        for forged in [
            Forge {
                issuer: Some("https://evil.okta.com"),
                ..Default::default()
            },
            Forge {
                audience: Some("another-client"),
                ..Default::default()
            },
            Forge {
                expired: true,
                ..Default::default()
            },
            Forge {
                wrong_key: true,
                ..Default::default()
            },
        ] {
            assert!(
                oidc.verify_id_token(&idp.token("user-a", forged), None)
                    .await
                    .is_err()
            );
        }
        let mut tampered = good.clone();
        tampered.push('x');
        assert!(oidc.verify_id_token(&tampered, Some("n1")).await.is_err());
        assert!(oidc.verify_id_token("not.a.token", None).await.is_err());
        // An unsigned token must never be accepted, whatever its claims say.
        let claims = good.split('.').nth(1).unwrap();
        let none = format!(
            "{}.{claims}.",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none","kid":"test-key"}"#)
        );
        assert!(oidc.verify_id_token(&none, Some("n1")).await.is_err());
    }
    #[tokio::test]
    async fn authentication_method_requirements_use_validated_claims() {
        let idp = Idp::start().await;
        let mut policy = idp.policy("https://relay.example");
        policy.required_amr = vec!["mfa".into()];
        let oidc = Oidc::discover(&policy, None).await.unwrap();
        assert!(
            oidc.verify_id_token(&idp.token("u", Forge::default()), None)
                .await
                .is_ok()
        );
        idp.inner.lock().unwrap().amr = vec!["pwd".into()];
        assert!(
            oidc.verify_id_token(&idp.token("u", Forge::default()), None)
                .await
                .is_err()
        );
        policy.required_amr.clear();
        policy.acr_values = Some("urn:okta:loa:2fa:any".into());
        let oidc = Oidc::discover(&policy, None).await.unwrap();
        assert!(
            oidc.verify_id_token(&idp.token("u", Forge::default()), None)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn discovery_and_refresh_fail_closed() {
        let idp = Idp::start().await;
        let mut wrong = idp.policy("https://relay.example");
        wrong.issuer = format!("{}/oauth2/other", idp.issuer);
        assert!(Oidc::discover(&wrong, None).await.is_err());
        let mut unreachable = idp.policy("https://relay.example");
        unreachable.issuer = "http://127.0.0.1:9".into();
        assert!(Oidc::discover(&unreachable, None).await.is_err());
        let oidc = Oidc::discover(&idp.policy("https://relay.example"), None)
            .await
            .unwrap();
        let refresh = idp.issue_refresh("user-a");
        assert_eq!(
            oidc.refresh(&refresh)
                .await
                .ok()
                .unwrap()
                .identity
                .principal
                .sub,
            "user-a"
        );
        idp.inner.lock().unwrap().outage = true;
        assert!(matches!(
            oidc.refresh(&refresh).await,
            Err(Failure::Unavailable)
        ));
        idp.inner.lock().unwrap().outage = false;
        idp.revoke("user-a");
        assert!(matches!(
            oidc.refresh(&refresh).await,
            Err(Failure::Rejected)
        ));
    }
    #[tokio::test]
    async fn code_exchange_requires_the_login_nonce_and_device_flow_waits_for_approval() {
        let idp = Idp::start().await;
        let oidc = Oidc::discover(&idp.policy("https://relay.example"), None)
            .await
            .unwrap();
        let verifier = crate::relay_crypto::random_token(32);
        let url = oidc
            .authorization_url(
                "https://relay.example/enterprise/callback",
                "s",
                "n",
                &verifier,
            )
            .unwrap();
        assert!(
            url.contains("code_challenge_method=S256")
                && url.contains("scope=openid+profile+offline_access")
        );
        assert!(!url.contains(&verifier));
        let code = idp.approve_code("user-a", "n");
        let tokens = oidc
            .exchange_code(
                &code,
                "https://relay.example/enterprise/callback",
                &verifier,
                "n",
            )
            .await
            .ok()
            .unwrap();
        assert!(tokens.refresh_token.is_some());
        // Codes are single use, and a token minted for another login is refused.
        assert!(
            oidc.exchange_code(
                &code,
                "https://relay.example/enterprise/callback",
                &verifier,
                "n"
            )
            .await
            .is_err()
        );
        let code = idp.approve_code("user-a", "someone-elses-nonce");
        assert!(matches!(
            oidc.exchange_code(
                &code,
                "https://relay.example/enterprise/callback",
                &verifier,
                "n"
            )
            .await,
            Err(Failure::Rejected)
        ));
        let approver = idp.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            approver.approve_devices("user-b");
        });
        let mut code = String::new();
        let tokens = oidc
            .device_login(|prompt| code = prompt.user_code.clone())
            .await
            .unwrap();
        assert_eq!(code, "TEST-CODE");
        assert_eq!(tokens.identity.principal.sub, "user-b");
    }
}
