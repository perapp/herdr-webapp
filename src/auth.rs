use axum::{
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use rand::RngCore;
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

pub struct Auth {
    token: String,
    sessions: Mutex<HashMap<String, Instant>>,
    attempts: Mutex<Vec<Instant>>,
    pub origin: Option<String>,
}

#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0,
            axum::Json(serde_json::json!({"error":{"message":self.1}})),
        )
            .into_response()
    }
}
pub fn error(status: StatusCode, message: impl Into<String>) -> ApiError {
    ApiError(status, message.into())
}
fn equal(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}
fn cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|s| s.trim().split_once('='))
        .find(|(k, _)| *k == "herdr_session")
        .map(|(_, v)| v.to_string())
}
impl Auth {
    pub fn new(token: String, origin: Option<String>) -> Self {
        Self {
            token,
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(Vec::new()),
            origin,
        }
    }
    pub fn authorized(&self, headers: &HeaderMap) -> bool {
        if let Some(v) = headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
        {
            return equal(&self.token, v);
        }
        let Some(session) = cookie(headers) else {
            return false;
        };
        self.sessions
            .lock()
            .unwrap()
            .get(&session)
            .is_some_and(|created| created.elapsed() < Duration::from_secs(60 * 60 * 24 * 7))
    }
    pub fn require(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        if self.authorized(headers) {
            Ok(())
        } else {
            Err(error(StatusCode::UNAUTHORIZED, "Authentication required"))
        }
    }
    /// Require an exact origin for all browser mutations and WebSocket handshakes.
    /// Explicit bearer API clients may omit Origin, never supply a foreign Origin.
    pub fn same_origin(&self, headers: &HeaderMap, websocket: bool) -> Result<(), ApiError> {
        let origin = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok());
        if origin.is_none()
            && !websocket
            && headers.contains_key(header::AUTHORIZATION)
            && self.authorized(headers)
        {
            return Ok(());
        }
        let expected = self.origin.clone().or_else(|| {
            headers
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .map(|h| format!("http://{h}"))
        });
        if origin.is_some() && origin == expected.as_deref() {
            Ok(())
        } else {
            Err(error(
                StatusCode::FORBIDDEN,
                "Origin does not match this server",
            ))
        }
    }
    pub fn login(&self, token: &str) -> Result<String, ApiError> {
        let mut attempts = self.attempts.lock().unwrap();
        attempts.retain(|t| t.elapsed() < Duration::from_secs(60));
        if attempts.len() >= 30 {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many sign-in attempts; try again in one minute",
            ));
        }
        attempts.push(Instant::now());
        if !equal(&self.token, token) {
            return Err(error(StatusCode::UNAUTHORIZED, "Invalid access token"));
        }
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let session = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, created| created.elapsed() < Duration::from_secs(60 * 60 * 24 * 7));
        if sessions.len() >= 1000 {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many active sessions",
            ));
        }
        sessions.insert(session.clone(), Instant::now());
        Ok(self.cookie(&session, false))
    }
    pub fn logout(&self, headers: &HeaderMap) -> String {
        if let Some(c) = cookie(headers) {
            self.sessions.lock().unwrap().remove(&c);
        }
        self.cookie("", true)
    }
    fn cookie(&self, value: &str, expired: bool) -> String {
        format!(
            "herdr_session={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
            if expired { 0 } else { 60 * 60 * 24 * 7 },
            if self
                .origin
                .as_deref()
                .is_some_and(|s| s.starts_with("https://"))
            {
                "; Secure"
            } else {
                ""
            }
        )
    }
}

pub async fn security_headers(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    for (name, value) in [
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("cache-control", "no-store"),
        (
            "permissions-policy",
            "camera=(), microphone=(), geolocation=()",
        ),
    ] {
        h.insert(
            axum::http::HeaderName::from_static(name),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cookies_origin_and_logout() {
        let a = Auth::new("secret".into(), None);
        let mut h = HeaderMap::new();
        h.insert("host", "host:8001".parse().unwrap());
        assert!(!a.authorized(&h));
        assert!(a.same_origin(&h, false).is_err());
        h.insert("origin", "http://evil".parse().unwrap());
        assert!(a.same_origin(&h, true).is_err());
        h.insert("origin", "http://host:8001".parse().unwrap());
        assert!(a.same_origin(&h, true).is_ok());
        assert!(a.login("wrong").is_err());
        let c = a.login("secret").unwrap();
        h.insert("cookie", c.split(';').next().unwrap().parse().unwrap());
        assert!(a.authorized(&h));
        a.logout(&h);
        assert!(!a.authorized(&h));
    }
    #[test]
    fn a_phone_using_the_lan_url_is_same_origin() {
        let auth = Auth::new("secret".into(), None);
        let mut headers = HeaderMap::new();
        headers.insert("host", "192.168.1.234:8001".parse().unwrap());
        headers.insert("origin", "http://192.168.1.234:8001".parse().unwrap());
        headers.insert(
            "user-agent",
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)"
                .parse()
                .unwrap(),
        );
        assert!(auth.same_origin(&headers, false).is_ok());
        assert!(auth.same_origin(&headers, true).is_ok());
        headers.insert("origin", "http://elsewhere.example".parse().unwrap());
        assert!(auth.same_origin(&headers, false).is_err());
    }
    #[test]
    fn bearer_cannot_bypass_foreign_origin() {
        let a = Auth::new("secret".into(), None);
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer secret".parse().unwrap());
        assert!(a.same_origin(&h, false).is_ok());
        assert!(a.same_origin(&h, true).is_err());
        h.insert("origin", "http://evil".parse().unwrap());
        assert!(a.same_origin(&h, false).is_err());
    }
}
