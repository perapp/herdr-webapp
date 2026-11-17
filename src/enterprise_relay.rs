//! Relay-side enterprise authorization service: Okta browser sessions, periodic
//! entitlement checks, administrator revocation and grant issuance.
//!
//! Public before sign-in: the static application shell, `/api/mode`, `/healthz`,
//! the connector bootstrap/download routes and `/enterprise/{login,callback,session}`.
//! None of them reaches a computer. Signaling (`join`, `register`) and TURN
//! credentials are refused without a verified, current Okta principal.
use crate::{
    enterprise_config::Policy,
    enterprise_grant::{Audit, Grant, fingerprint},
    enterprise_oidc::{Failure, Identity, Oidc, Principal},
    relay_crypto,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 1024;
const MAX_PENDING_PER_IP: usize = 8;
const MAX_SESSIONS: usize = 4096;
const TICK: Duration = Duration::from_secs(5);

/// Why access is refused. The code is shown to the affected user only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denial {
    LoginRequired,
    OktaUnavailable,
    OwnerMismatch,
    Revoked,
}
impl Denial {
    pub fn code(self) -> &'static str {
        match self {
            Self::LoginRequired => "enterprise-login-required",
            Self::OktaUnavailable => "enterprise-okta-unavailable",
            Self::OwnerMismatch => "enterprise-owner-mismatch",
            Self::Revoked => "enterprise-revoked",
        }
    }
}
impl std::fmt::Display for Denial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Denial {}

struct Pending {
    binding: String,
    nonce: String,
    verifier: String,
    return_to: String,
    ip: Option<IpAddr>,
    created: Instant,
}
struct WebSession {
    identity: Identity,
    refresh_token: String,
    created: Instant,
    verified: Instant,
    refreshing: bool,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Revocations {
    #[serde(default)]
    subjects: HashSet<String>,
    #[serde(default)]
    machines: HashSet<String>,
    #[serde(default)]
    browser_keys: HashSet<String>,
}
#[derive(Default)]
struct Inner {
    pending: HashMap<String, Pending>,
    sessions: HashMap<String, WebSession>,
    revocations: Revocations,
    revocations_seen: Option<SystemTime>,
    revocations_loaded: bool,
    /// An unreadable revocation list denies everything rather than nothing.
    revocations_broken: bool,
}
pub struct Enterprise {
    pub policy: Policy,
    oidc: Oidc,
    origin: String,
    secure: bool,
    grant_key: relay_crypto::Identity,
    inner: Mutex<Inner>,
    /// Wakes the relay's enforcement sweep after logout or revocation.
    pub changed: tokio::sync::Notify,
    pub audit: Audit,
}

fn hash(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, value)| *key == name && value.len() == 43)
        .map(|(_, value)| value)
}
/// Only the application's own secret-free routes may be a post-login destination.
fn safe_return(value: Option<&str>) -> String {
    let value = value.unwrap_or("/");
    let (path, query) = value.split_once('?').unwrap_or((value, ""));
    let application = path == "/"
        || path == "/launch"
        || path.strip_prefix("/connections/").is_some_and(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
    if application && matches!(query, "" | "forceTurn=1") {
        value.to_owned()
    } else {
        "/".into()
    }
}

impl Enterprise {
    pub async fn start(policy: Policy, origin: String) -> Result<Arc<Self>> {
        ensure!(
            policy.relay_url == origin,
            "enterprise relay_url ({}) does not match the public origin ({origin})",
            policy.relay_url
        );
        let secret = policy
            .client_secret_file
            .as_ref()
            .map(|path| -> Result<String> {
                let text = String::from_utf8(relay_crypto::read_private(path, 4096)?)?;
                let text = text.trim().to_owned();
                ensure!(!text.is_empty(), "Okta client secret file is empty");
                Ok(text)
            })
            .transpose()
            .context("Okta client secret must be a private, owner-only file")?;
        let oidc = Oidc::discover(&policy, secret).await?;
        let this = Arc::new(Self {
            secure: origin.starts_with("https://"),
            origin,
            oidc,
            grant_key: relay_crypto::Identity::ephemeral(),
            inner: Default::default(),
            changed: tokio::sync::Notify::new(),
            audit: Audit::stderr(),
            policy,
        });
        this.reload_revocations();
        ensure!(
            !this.inner.lock().unwrap().revocations_broken,
            "enterprise revocations_file is unreadable or malformed"
        );
        let worker = Arc::downgrade(&this);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(TICK).await;
                let Some(this) = worker.upgrade() else { break };
                this.maintain().await;
            }
        });
        Ok(this)
    }
    pub fn grant_key(&self) -> String {
        self.grant_key.public_key()
    }
    fn session_cookie(&self) -> &'static str {
        // The __Host- prefix needs Secure; loopback HTTP exists for development only.
        if self.secure {
            "__Host-herdr-enterprise"
        } else {
            "herdr-enterprise"
        }
    }
    fn login_cookie(&self) -> &'static str {
        if self.secure {
            "__Host-herdr-enterprise-login"
        } else {
            "herdr-enterprise-login"
        }
    }
    fn set_cookie(&self, name: &str, value: &str, same_site: &str, max_age: u64) -> HeaderValue {
        format!(
            "{name}={value}; Path=/; HttpOnly; SameSite={same_site}; Max-Age={max_age}{}",
            if self.secure { "; Secure" } else { "" }
        )
        .parse()
        .expect("generated cookie")
    }
    /// Identifies the Okta browser session for a request; never the cookie itself.
    pub fn session_key(&self, headers: &HeaderMap) -> Option<String> {
        cookie(headers, self.session_cookie()).map(hash)
    }
    fn redirect_uri(&self) -> String {
        format!("{}/enterprise/callback", self.origin)
    }

    fn reload_revocations(&self) {
        let Some(path) = &self.policy.revocations_file else {
            return;
        };
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let mut inner = self.inner.lock().unwrap();
        if modified == inner.revocations_seen
            && !inner.revocations_broken
            && (modified.is_some() || inner.revocations_loaded)
        {
            return;
        }
        inner.revocations_loaded = true;
        let parsed = match std::fs::read_to_string(path) {
            Ok(text) if text.len() <= 1024 * 1024 => toml::from_str::<Revocations>(&text).ok(),
            // No file yet means nothing has been revoked.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Default::default()),
            _ => None,
        };
        let broken = parsed.is_none();
        if broken != inner.revocations_broken || modified != inner.revocations_seen {
            self.audit.log(
                if broken {
                    "revocations-unreadable"
                } else {
                    "revocations-loaded"
                },
                json!({}),
            );
        }
        inner.revocations_broken = broken;
        inner.revocations_seen = modified;
        if let Some(parsed) = parsed {
            inner.revocations = parsed;
        }
        drop(inner);
        self.changed.notify_waiters();
    }
    fn revoked(inner: &Inner, principal: Option<&Principal>, machine: &str, browser: &str) -> bool {
        inner.revocations_broken
            || principal.is_some_and(|p| inner.revocations.subjects.contains(&p.sub))
            || inner.revocations.machines.contains(machine)
            || inner.revocations.browser_keys.contains(browser)
    }

    /// The single browser-side decision, repeated on join and on every renewal:
    /// current Okta session, same user as the enrolled owner, nothing revoked.
    pub fn authorize_browser(
        &self,
        session: Option<&str>,
        owner: &Principal,
        machine: &str,
        browser_key: &str,
    ) -> Result<Principal, Denial> {
        let inner = self.inner.lock().unwrap();
        let found = session
            .and_then(|key| inner.sessions.get(key))
            .filter(|s| s.created.elapsed() < Duration::from_secs(self.policy.session_max_seconds))
            .ok_or(Denial::LoginRequired)?;
        if found.verified.elapsed() >= Duration::from_secs(self.policy.recheck_seconds) {
            return Err(Denial::OktaUnavailable);
        }
        let principal = &found.identity.principal;
        if Self::revoked(&inner, Some(principal), machine, browser_key)
            || inner.revocations.subjects.contains(&owner.sub)
        {
            return Err(Denial::Revoked);
        }
        if principal != owner {
            return Err(Denial::OwnerMismatch);
        }
        Ok(principal.clone())
    }
    pub fn grant(
        &self,
        principal: &Principal,
        machine: &str,
        browser_key: &str,
        context: &str,
        session_id: &str,
    ) -> Value {
        Grant::new(
            principal,
            machine,
            browser_key,
            context,
            session_id,
            self.policy.grant_lifetime_seconds,
        )
        .sign(&self.grant_key)
    }
    /// A connector proves its owner with a current Okta ID token for this client.
    pub async fn verify_connector(&self, id_token: &str, machine: &str) -> Result<Principal> {
        let identity = self.oidc.verify_id_token(id_token, None).await?;
        // Each proof must be recent: an old token cannot keep a registration alive.
        ensure!(
            crate::enterprise_grant::now().saturating_sub(identity.issued_at)
                < self.policy.recheck_seconds + 60,
            "owner proof is too old"
        );
        let inner = self.inner.lock().unwrap();
        ensure!(
            !Self::revoked(&inner, Some(&identity.principal), machine, ""),
            "revoked"
        );
        Ok(identity.principal)
    }
    pub fn connector_revoked(&self, owner: &Principal, machine: &str) -> bool {
        Self::revoked(&self.inner.lock().unwrap(), Some(owner), machine, "")
    }

    async fn maintain(self: &Arc<Self>) {
        self.reload_revocations();
        let recheck = Duration::from_secs(self.policy.recheck_seconds);
        let maximum = Duration::from_secs(self.policy.session_max_seconds);
        let due: Vec<(String, String)> = {
            let mut inner = self.inner.lock().unwrap();
            inner
                .pending
                .retain(|_, p| p.created.elapsed() < LOGIN_TIMEOUT);
            let before = inner.sessions.len();
            inner.sessions.retain(|_, s| s.created.elapsed() < maximum);
            if inner.sessions.len() != before {
                self.changed.notify_waiters();
            }
            inner
                .sessions
                .iter_mut()
                .filter(|(_, s)| !s.refreshing && s.verified.elapsed() >= recheck / 2)
                .take(32)
                .map(|(key, s)| {
                    s.refreshing = true;
                    (key.clone(), s.refresh_token.clone())
                })
                .collect()
        };
        let results =
            futures_util::future::join_all(due.into_iter().map(|(key, token)| async move {
                let result = self.oidc.refresh(&token).await;
                (key, result)
            }))
            .await;
        for (key, result) in results {
            let mut inner = self.inner.lock().unwrap();
            let Some(session) = inner.sessions.get_mut(&key) else {
                continue;
            };
            session.refreshing = false;
            let sub = session.identity.principal.sub.clone();
            match result {
                Ok(tokens) if tokens.identity.principal == session.identity.principal => {
                    session.verified = Instant::now();
                    if let Some(token) = tokens.refresh_token {
                        session.refresh_token = token;
                    }
                    drop(inner);
                    self.audit.log(
                        "authorization-renewed",
                        json!({"sub":sub,"session":fingerprint(&key)}),
                    );
                }
                // Okta said no (or answered for someone else): end access now.
                Ok(_) | Err(Failure::Rejected) => {
                    inner.sessions.remove(&key);
                    drop(inner);
                    self.audit.log(
                        "authorization-revoked",
                        json!({"sub":sub,"session":fingerprint(&key),"reason":"okta-rejected"}),
                    );
                    self.changed.notify_waiters();
                }
                // Keep trying; grants stop once the last verification is too old.
                Err(Failure::Unavailable) => {
                    drop(inner);
                    self.audit.log(
                        "authorization-renewal-failed",
                        json!({"sub":sub,"session":fingerprint(&key),"reason":"okta-unavailable"}),
                    );
                }
            }
        }
    }

    #[cfg(test)]
    pub fn test_session(&self, principal: Principal, refresh_token: String) -> String {
        let value = relay_crypto::random_token(32);
        self.inner.lock().unwrap().sessions.insert(
            hash(&value),
            WebSession {
                identity: Identity {
                    principal,
                    name: "Test".into(),
                    issued_at: 0,
                },
                refresh_token,
                created: Instant::now(),
                verified: Instant::now(),
                refreshing: false,
            },
        );
        value
    }
    #[cfg(test)]
    pub fn test_age(&self, verified: Duration) {
        for session in self.inner.lock().unwrap().sessions.values_mut() {
            session.verified = Instant::now() - verified;
        }
    }
    #[cfg(test)]
    pub async fn test_maintain(self: &Arc<Self>) {
        self.maintain().await;
    }
    #[cfg(test)]
    pub fn test_cookie_name(&self) -> &'static str {
        self.session_cookie()
    }
}

pub fn routes(state: Arc<Enterprise>) -> Router {
    Router::new()
        .route("/enterprise/login", get(login))
        .route("/enterprise/callback", get(callback))
        .route("/enterprise/session", get(session))
        .route("/enterprise/logout", post(logout))
        .with_state(state)
}
fn redirect(location: &str) -> Response {
    (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response()
}
fn failed(e: &Enterprise, code: &str) -> Response {
    let mut response = redirect(&format!("/?enterprise_error={code}"));
    response.headers_mut().append(
        header::SET_COOKIE,
        e.set_cookie(e.login_cookie(), "", "Lax", 0),
    );
    response
}
async fn login(
    State(e): State<Arc<Enterprise>>,
    client: Option<axum::Extension<ConnectInfo<SocketAddr>>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ip = client.map(|c| c.0.0.ip());
    let state = relay_crypto::random_token(32);
    let nonce = relay_crypto::random_token(32);
    let verifier = relay_crypto::random_token(48);
    let binding = relay_crypto::random_token(32);
    let Ok(url) = e
        .oidc
        .authorization_url(&e.redirect_uri(), &state, &nonce, &verifier)
    else {
        return failed(&e, "failed");
    };
    {
        let mut inner = e.inner.lock().unwrap();
        inner
            .pending
            .retain(|_, p| p.created.elapsed() < LOGIN_TIMEOUT);
        if inner.pending.len() >= MAX_PENDING
            || inner.pending.values().filter(|p| p.ip == ip).count() >= MAX_PENDING_PER_IP
        {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        inner.pending.insert(
            state,
            Pending {
                binding: hash(&binding),
                nonce,
                verifier,
                return_to: safe_return(query.get("return").map(String::as_str)),
                ip,
                created: Instant::now(),
            },
        );
    }
    let mut response = redirect(&url);
    // Binds the callback to the browser that started this login.
    response.headers_mut().append(
        header::SET_COOKIE,
        e.set_cookie(e.login_cookie(), &binding, "Lax", LOGIN_TIMEOUT.as_secs()),
    );
    response
}
async fn callback(
    State(e): State<Arc<Enterprise>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let pending = query
        .get("state")
        .and_then(|state| e.inner.lock().unwrap().pending.remove(state));
    // One use: the state is consumed whether or not the rest succeeds.
    let Some(pending) = pending.filter(|p| {
        p.created.elapsed() < LOGIN_TIMEOUT
            && cookie(&headers, e.login_cookie()).map(hash).as_deref() == Some(&p.binding)
    }) else {
        e.audit
            .log("login-failed", json!({"reason":"expired-or-foreign-state"}));
        return failed(&e, "expired");
    };
    let Some(code) = query.get("code").filter(|c| c.len() <= 2048) else {
        e.audit
            .log("login-failed", json!({"reason":"denied-by-okta"}));
        return failed(&e, "denied");
    };
    let tokens = match e
        .oidc
        .exchange_code(code, &e.redirect_uri(), &pending.verifier, &pending.nonce)
        .await
    {
        Ok(tokens) => tokens,
        Err(failure) => {
            e.audit
                .log("login-failed", json!({"reason":failure.to_string()}));
            return failed(
                &e,
                if matches!(failure, Failure::Rejected) {
                    "rejected"
                } else {
                    "unavailable"
                },
            );
        }
    };
    let sub = tokens.identity.principal.sub.clone();
    // Without a refresh token Okta revocation could not be detected: refuse.
    let Some(refresh_token) = tokens.refresh_token else {
        e.audit.log(
            "login-failed",
            json!({"sub":sub,"reason":"no-refresh-token"}),
        );
        return failed(&e, "configuration");
    };
    let value = relay_crypto::random_token(32);
    {
        let mut inner = e.inner.lock().unwrap();
        if inner.revocations_broken || inner.revocations.subjects.contains(&sub) {
            drop(inner);
            e.audit
                .log("login-failed", json!({"sub":sub,"reason":"revoked"}));
            return failed(&e, "revoked");
        }
        if inner.sessions.len() >= MAX_SESSIONS {
            return failed(&e, "unavailable");
        }
        inner.sessions.insert(
            hash(&value),
            WebSession {
                identity: tokens.identity,
                refresh_token,
                created: Instant::now(),
                verified: Instant::now(),
                refreshing: false,
            },
        );
    }
    e.audit.log(
        "login",
        json!({"sub":sub,"session":fingerprint(&hash(&value))}),
    );
    let mut response = redirect(&pending.return_to);
    let headers = response.headers_mut();
    headers.append(
        header::SET_COOKIE,
        e.set_cookie(e.login_cookie(), "", "Lax", 0),
    );
    headers.append(
        header::SET_COOKIE,
        e.set_cookie(
            e.session_cookie(),
            &value,
            "Strict",
            e.policy.session_max_seconds,
        ),
    );
    response
}
async fn session(State(e): State<Arc<Enterprise>>, headers: HeaderMap) -> Response {
    let key = e.session_key(&headers);
    let inner = e.inner.lock().unwrap();
    let maximum = Duration::from_secs(e.policy.session_max_seconds);
    match key
        .and_then(|key| inner.sessions.get(&key))
        .filter(|s| s.created.elapsed() < maximum)
    {
        Some(found) => Json(json!({"authenticated":true,"name":found.identity.name,
            "expiresIn":maximum.saturating_sub(found.created.elapsed()).as_secs()}))
        .into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"authenticated":false})),
        )
            .into_response(),
    }
}
async fn logout(State(e): State<Arc<Enterprise>>, headers: HeaderMap) -> Response {
    let origins: Vec<_> = headers.get_all(header::ORIGIN).iter().collect();
    if origins.len() != 1 || origins[0].as_bytes() != e.origin.as_bytes() {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(key) = e.session_key(&headers)
        && let Some(found) = e.inner.lock().unwrap().sessions.remove(&key)
    {
        e.audit.log(
            "logout",
            json!({"sub":found.identity.principal.sub,"session":fingerprint(&key)}),
        );
        e.changed.notify_waiters();
    }
    let mut response = Json(json!({"authenticated":false})).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        e.set_cookie(e.session_cookie(), "", "Strict", 0),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enterprise_oidc::mock::Idp;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    const ORIGIN: &str = "https://relay.example";
    async fn setup() -> (Arc<Idp>, Arc<Enterprise>, Router) {
        let idp = Idp::start().await;
        let e = Enterprise::start(idp.policy(ORIGIN), ORIGIN.into())
            .await
            .unwrap();
        (idp, e.clone(), routes(e))
    }
    fn cookies(response: &Response) -> Vec<String> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect()
    }
    fn pair(set_cookie: &str) -> String {
        set_cookie.split(';').next().unwrap().to_owned()
    }
    async fn get(app: &Router, uri: &str, cookie: Option<&str>) -> Response {
        let mut request = Request::get(uri);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        app.clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }
    /// Start a login and return (state, nonce, login cookie pair).
    async fn begin(app: &Router, path: &str) -> (String, String, String) {
        let response = get(app, path, None).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response.headers()[header::LOCATION].to_str().unwrap();
        let url = url::Url::parse(location).unwrap();
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            query["redirect_uri"],
            format!("{ORIGIN}/enterprise/callback")
        );
        let set = cookies(&response);
        assert!(
            set[0].contains("HttpOnly")
                && set[0].contains("Secure")
                && set[0].contains("SameSite=Lax")
        );
        (
            query["state"].clone(),
            query["nonce"].clone(),
            pair(&set[0]),
        )
    }
    async fn sign_in(idp: &Idp, app: &Router, sub: &str) -> String {
        let (state, nonce, login) = begin(app, "/enterprise/login").await;
        let code = idp.approve_code(sub, &nonce);
        let response = get(
            app,
            &format!("/enterprise/callback?code={code}&state={state}"),
            Some(&login),
        )
        .await;
        assert_eq!(response.headers()[header::LOCATION], "/");
        let set = cookies(&response);
        let session = set
            .iter()
            .find(|c| c.starts_with("__Host-herdr-enterprise="))
            .unwrap();
        assert!(
            session.contains("HttpOnly")
                && session.contains("Secure")
                && session.contains("SameSite=Strict")
                && session.contains("Path=/")
        );
        pair(session)
    }
    fn key_of(e: &Enterprise, cookie_pair: &str) -> String {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie_pair.parse().unwrap());
        e.session_key(&headers).unwrap()
    }
    fn browser_key() -> String {
        relay_crypto::Identity::ephemeral().public_key()
    }

    #[tokio::test]
    async fn login_round_trip_creates_a_session_and_preserves_only_safe_destinations() {
        let (idp, _e, app) = setup().await;
        assert_eq!(
            get(&app, "/enterprise/session", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let session = sign_in(&idp, &app, "user-a").await;
        let response = get(&app, "/enterprise/session", Some(&session)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["name"], "User user-a");
        for (requested, expected) in [
            ("/connections/abc_DEF-1", "/connections/abc_DEF-1"),
            (
                "/connections/abc%3FforceTurn=1",
                "/connections/abc?forceTurn=1",
            ),
            ("/launch", "/launch"),
            ("https://evil.example/", "/"),
            ("//evil.example", "/"),
            ("/connections/a/b", "/"),
            ("/%23pair=secret", "/"),
            ("/api/mode", "/"),
        ] {
            let (state, nonce, login) =
                begin(&app, &format!("/enterprise/login?return={requested}")).await;
            let code = idp.approve_code("user-a", &nonce);
            let response = get(
                &app,
                &format!("/enterprise/callback?code={code}&state={state}"),
                Some(&login),
            )
            .await;
            assert_eq!(
                response.headers()[header::LOCATION],
                expected,
                "{requested}"
            );
        }
    }
    #[tokio::test]
    async fn callback_rejects_foreign_replayed_denied_and_mismatched_logins() {
        let (idp, e, app) = setup().await;
        let location = |r: &Response| r.headers()[header::LOCATION].to_str().unwrap().to_owned();
        // Another browser cannot complete a login it did not start.
        let (state, nonce, _login) = begin(&app, "/enterprise/login").await;
        let code = idp.approve_code("user-a", &nonce);
        let uri = format!("/enterprise/callback?code={code}&state={state}");
        assert_eq!(
            location(&get(&app, &uri, None).await),
            "/?enterprise_error=expired"
        );
        // State is single use, even for the right browser afterwards.
        let (state, nonce, login) = begin(&app, "/enterprise/login").await;
        let code = idp.approve_code("user-a", &nonce);
        let uri = format!("/enterprise/callback?code={code}&state={state}");
        assert_eq!(location(&get(&app, &uri, Some(&login)).await), "/");
        assert_eq!(
            location(&get(&app, &uri, Some(&login)).await),
            "/?enterprise_error=expired"
        );
        // Okta denial, and a code issued for a different login's nonce.
        let (state, _nonce, login) = begin(&app, "/enterprise/login").await;
        let uri = format!("/enterprise/callback?error=access_denied&state={state}");
        assert_eq!(
            location(&get(&app, &uri, Some(&login)).await),
            "/?enterprise_error=denied"
        );
        let (state, _nonce, login) = begin(&app, "/enterprise/login").await;
        let code = idp.approve_code("user-a", "nonce-of-another-login");
        let uri = format!("/enterprise/callback?code={code}&state={state}");
        let response = get(&app, &uri, Some(&login)).await;
        assert_eq!(location(&response), "/?enterprise_error=rejected");
        assert!(
            !cookies(&response)
                .iter()
                .any(|c| c.starts_with("__Host-herdr-enterprise="))
        );
        // An identity provider outage never produces a session.
        let (state, nonce, login) = begin(&app, "/enterprise/login").await;
        let code = idp.approve_code("user-a", &nonce);
        idp.inner.lock().unwrap().outage = true;
        let uri = format!("/enterprise/callback?code={code}&state={state}");
        assert_eq!(
            location(&get(&app, &uri, Some(&login)).await),
            "/?enterprise_error=unavailable"
        );
        assert_eq!(e.inner.lock().unwrap().sessions.len(), 1);
    }
    #[tokio::test]
    async fn only_the_enrolled_owner_with_a_current_session_is_authorized() {
        let (idp, e, app) = setup().await;
        let owner = Principal {
            iss: idp.issuer.clone(),
            sub: "user-a".into(),
        };
        let browser = browser_key();
        let a = key_of(&e, &sign_in(&idp, &app, "user-a").await);
        let b = key_of(&e, &sign_in(&idp, &app, "user-b").await);
        assert_eq!(
            e.authorize_browser(Some(&a), &owner, "m", &browser),
            Ok(owner.clone())
        );
        // A different, perfectly valid corporate user is refused.
        assert_eq!(
            e.authorize_browser(Some(&b), &owner, "m", &browser),
            Err(Denial::OwnerMismatch)
        );
        // Same subject string from another issuer is a different principal.
        let foreign = Principal {
            iss: "https://other.okta.com".into(),
            sub: "user-a".into(),
        };
        assert_eq!(
            e.authorize_browser(Some(&a), &foreign, "m", &browser),
            Err(Denial::OwnerMismatch)
        );
        assert_eq!(
            e.authorize_browser(None, &owner, "m", &browser),
            Err(Denial::LoginRequired)
        );
        assert_eq!(
            e.authorize_browser(Some("unknown"), &owner, "m", &browser),
            Err(Denial::LoginRequired)
        );
    }
    #[tokio::test]
    async fn okta_revocation_and_outage_end_authorization_within_the_recheck_bound() {
        let (idp, e, app) = setup().await;
        let owner = Principal {
            iss: idp.issuer.clone(),
            sub: "user-a".into(),
        };
        let browser = browser_key();
        let a = key_of(&e, &sign_in(&idp, &app, "user-a").await);
        // Healthy renewal keeps the session verified.
        e.test_age(Duration::from_secs(200));
        e.test_maintain().await;
        assert!(e.authorize_browser(Some(&a), &owner, "m", &browser).is_ok());
        // Outage: retried, but grants stop once verification is older than the bound.
        idp.inner.lock().unwrap().outage = true;
        e.test_age(Duration::from_secs(200));
        e.test_maintain().await;
        assert!(e.authorize_browser(Some(&a), &owner, "m", &browser).is_ok());
        e.test_age(Duration::from_secs(301));
        e.test_maintain().await;
        assert_eq!(
            e.authorize_browser(Some(&a), &owner, "m", &browser),
            Err(Denial::OktaUnavailable)
        );
        // Recovery restores access without a new login.
        idp.inner.lock().unwrap().outage = false;
        e.test_maintain().await;
        assert!(e.authorize_browser(Some(&a), &owner, "m", &browser).is_ok());
        // Okta disables the user: the next check removes the session entirely.
        idp.revoke("user-a");
        e.test_age(Duration::from_secs(200));
        e.test_maintain().await;
        assert_eq!(
            e.authorize_browser(Some(&a), &owner, "m", &browser),
            Err(Denial::LoginRequired)
        );
    }
    #[tokio::test]
    async fn administrator_revocation_logout_and_broken_lists_deny() {
        let idp = Idp::start().await;
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("revocations.toml");
        let mut policy = idp.policy(ORIGIN);
        policy.revocations_file = Some(list.clone());
        let e = Enterprise::start(policy.clone(), ORIGIN.into())
            .await
            .unwrap();
        let app = routes(e.clone());
        let owner = Principal {
            iss: idp.issuer.clone(),
            sub: "user-a".into(),
        };
        let (browser, other) = (browser_key(), browser_key());
        let cookie = sign_in(&idp, &app, "user-a").await;
        let a = key_of(&e, &cookie);
        assert!(e.authorize_browser(Some(&a), &owner, "m", &browser).is_ok());
        std::fs::write(
            &list,
            format!("browser_keys = [\"{browser}\"]\nmachines = [\"bad-machine\"]\n"),
        )
        .unwrap();
        e.reload_revocations();
        assert_eq!(
            e.authorize_browser(Some(&a), &owner, "m", &browser),
            Err(Denial::Revoked)
        );
        assert_eq!(
            e.authorize_browser(Some(&a), &owner, "bad-machine", &other),
            Err(Denial::Revoked)
        );
        assert!(e.authorize_browser(Some(&a), &owner, "m", &other).is_ok());
        assert!(e.connector_revoked(&owner, "bad-machine") && !e.connector_revoked(&owner, "m"));
        std::fs::write(&list, "subjects = [\"user-a\"]\n").unwrap();
        e.inner.lock().unwrap().revocations_seen = None;
        e.reload_revocations();
        assert_eq!(
            e.authorize_browser(Some(&a), &owner, "m", &other),
            Err(Denial::Revoked)
        );
        assert!(
            e.verify_connector(&idp.token("user-a", Default::default()), "m")
                .await
                .is_err()
        );
        assert!(
            e.verify_connector(&idp.token("user-b", Default::default()), "m")
                .await
                .is_ok()
        );
        // A damaged list denies everyone until repaired, and blocks startup.
        std::fs::write(&list, "subjects = [").unwrap();
        e.inner.lock().unwrap().revocations_seen = None;
        e.reload_revocations();
        assert!(
            e.verify_connector(&idp.token("user-b", Default::default()), "m")
                .await
                .is_err()
        );
        assert!(Enterprise::start(policy, ORIGIN.into()).await.is_err());
        std::fs::remove_file(&list).unwrap();
        e.reload_revocations();
        assert!(e.authorize_browser(Some(&a), &owner, "m", &other).is_ok());
        // Logout needs the exact Origin and ends the session immediately.
        let post = |origin: Option<&'static str>| {
            let mut request = Request::post("/enterprise/logout").header(header::COOKIE, &cookie);
            if let Some(origin) = origin {
                request = request.header(header::ORIGIN, origin);
            }
            app.clone().oneshot(request.body(Body::empty()).unwrap())
        };
        assert_eq!(post(None).await.unwrap().status(), StatusCode::FORBIDDEN);
        assert_eq!(
            post(Some("https://evil.example")).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        assert!(e.authorize_browser(Some(&a), &owner, "m", &other).is_ok());
        assert_eq!(post(Some(ORIGIN)).await.unwrap().status(), StatusCode::OK);
        assert_eq!(
            e.authorize_browser(Some(&a), &owner, "m", &other),
            Err(Denial::LoginRequired)
        );
    }
    #[tokio::test]
    async fn connector_identity_requires_a_valid_token_for_this_client() {
        let (idp, e, _app) = setup().await;
        use crate::enterprise_oidc::mock::Forge;
        let principal = e
            .verify_connector(&idp.token("user-a", Forge::default()), "m")
            .await
            .unwrap();
        assert_eq!(
            principal,
            Principal {
                iss: idp.issuer.clone(),
                sub: "user-a".into()
            }
        );
        for forged in [
            Forge {
                expired: true,
                ..Default::default()
            },
            Forge {
                audience: Some("other-app"),
                ..Default::default()
            },
            Forge {
                issuer: Some("https://evil.okta.com"),
                ..Default::default()
            },
            Forge {
                wrong_key: true,
                ..Default::default()
            },
        ] {
            assert!(
                e.verify_connector(&idp.token("user-a", forged), "m")
                    .await
                    .is_err()
            );
        }
        assert!(
            Enterprise::start(idp.policy("https://other.example"), ORIGIN.into())
                .await
                .is_err()
        );
    }
}
