use anyhow::{Context, bail};
use axum::{
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

const SESSION_LIFETIME: u64 = 60 * 60 * 24 * 7;
const MAX_SESSIONS: usize = 1000;

#[derive(Default)]
struct Sessions {
    entries: HashMap<String, u64>,
    failed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionFile {
    version: u32,
    token_hash: String,
    sessions: HashMap<String, u64>,
}

fn hash(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now() -> anyhow::Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn valid_time(created: u64, now: u64) -> bool {
    created != 0 && created <= now && now - created < SESSION_LIFETIME
}

fn private_file(path: &Path) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    validate_private_file(&file)?;
    Ok(file)
}

fn validate_private_file(file: &File) -> anyhow::Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.permissions().mode() & 0o777 != 0o600
        || meta.uid() != unsafe { libc::getuid() }
        || meta.nlink() != 1
    {
        bail!("session store must be a private, user-owned regular file with mode 0600");
    }
    Ok(())
}

/// The lock inode is stable: never rename or unlink this sidecar, even on drop.
fn lock_store(path: &Path) -> anyhow::Result<File> {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&lock_path)
    {
        Ok(file) => {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            file
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => private_file(&lock_path)?,
        Err(e) => return Err(e.into()),
    };
    validate_private_file(&file)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("session store is locked by another server");
    }
    // Refuse a replaced path rather than running with a lock on an obsolete inode.
    let held = file.metadata()?;
    let named = std::fs::symlink_metadata(&lock_path)?;
    if held.dev() != named.dev() || held.ino() != named.ino() || !named.is_file() {
        bail!("session store lock was replaced");
    }
    Ok(file)
}

/// Replace the store atomically, syncing both the contents and directory entry.
fn save_file(path: &Path, data: &SessionFile) -> anyhow::Result<()> {
    match private_file(path) {
        Ok(_) => {}
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(e) => return Err(e),
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let directory = File::open(parent)?;
    let mut random = [0u8; 32];
    rand::rng().fill_bytes(&mut random);
    let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let temporary = parent.join(format!(".herdr-sessions-{name}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    let result = (|| {
        // The creation mode is filtered by umask; require exactly 0600 on disk.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(&serde_json::to_vec(data)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        directory.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

pub struct Auth {
    token: String,
    sessions: Mutex<Sessions>,
    store: Option<PathBuf>,
    _store_lock: Option<File>,
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
    #[cfg(test)]
    pub fn new(token: String, origin: Option<String>) -> Self {
        Self {
            token,
            sessions: Mutex::new(Sessions::default()),
            store: None,
            _store_lock: None,
            attempts: Mutex::new(Vec::new()),
            origin,
        }
    }
    pub fn persistent(
        token: String,
        origin: Option<String>,
        path: PathBuf,
    ) -> anyhow::Result<Self> {
        // Acquire before reading or rewriting any persisted sessions.
        let store_lock = lock_store(&path).context("lock private session store")?;
        let current = now().context("read session clock")?;
        let mut entries = HashMap::new();
        match private_file(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .context("read private session store")?;
                if bytes.len() > 1024 * 1024 {
                    bail!("session store is too large");
                }
                // Never attach parser errors: they may quote sensitive file contents.
                let data: SessionFile = serde_json::from_slice(&bytes)
                    .map_err(|_| anyhow::anyhow!("invalid session store"))?;
                if data.version != 1 || data.sessions.len() > MAX_SESSIONS {
                    bail!("invalid session store");
                }
                if equal(&data.token_hash, &hash(&token)) {
                    for (id, created) in data.sessions {
                        if id.len() != 64
                            || !id
                                .bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                            || created == 0
                            || created > current
                        {
                            bail!("invalid session store");
                        }
                        if valid_time(created, current) {
                            entries.insert(id, created);
                        }
                    }
                }
            }
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(_) => bail!("cannot open private session store"),
        }
        let auth = Self {
            token,
            origin,
            store: Some(path),
            _store_lock: Some(store_lock),
            sessions: Mutex::new(Sessions {
                entries,
                failed: false,
            }),
            attempts: Mutex::new(Vec::new()),
        };
        auth.save(&auth.sessions.lock().unwrap().entries)
            .context("persist private session store")?;
        Ok(auth)
    }

    fn save(&self, entries: &HashMap<String, u64>) -> anyhow::Result<()> {
        if let Some(path) = &self.store {
            save_file(
                path,
                &SessionFile {
                    version: 1,
                    token_hash: hash(&self.token),
                    sessions: entries.clone(),
                },
            )?;
        }
        Ok(())
    }

    fn storage_error() -> ApiError {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Session storage unavailable; restart the server after fixing its private session store",
        )
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
        let sessions = self.sessions.lock().unwrap();
        !sessions.failed
            && now().is_ok_and(|current| {
                sessions
                    .entries
                    .get(&hash(&session))
                    .is_some_and(|created| valid_time(*created, current))
            })
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
        if sessions.failed {
            return Err(Self::storage_error());
        }
        let current = now().map_err(|_| Self::storage_error())?;
        let mut next = sessions.entries.clone();
        next.retain(|_, created| valid_time(*created, current));
        if next.len() >= MAX_SESSIONS {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many active sessions",
            ));
        }
        next.insert(hash(&session), current);
        if self.save(&next).is_err() {
            sessions.failed = true;
            sessions.entries.clear();
            return Err(Self::storage_error());
        }
        sessions.entries = next;
        Ok(self.cookie(&session, false))
    }
    pub fn logout(&self, headers: &HeaderMap) -> Result<String, ApiError> {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.failed {
            return Err(Self::storage_error());
        }
        let mut next = sessions.entries.clone();
        if let Some(c) = cookie(headers) {
            next.remove(&hash(&c));
        }
        if self.save(&next).is_err() {
            sessions.failed = true;
            sessions.entries.clear();
            return Err(Self::storage_error());
        }
        sessions.entries = next;
        Ok(self.cookie("", true))
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
    fn headers_for(cookie: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("cookie", cookie.split(';').next().unwrap().parse().unwrap());
        h
    }

    fn persistent(path: &Path, token: &str) -> anyhow::Result<Auth> {
        Auth::persistent(token.into(), None, path.into())
    }

    #[test]
    fn exclusive_store_lock_is_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions");
        let auth = persistent(&path, "secret").unwrap();
        let headers = headers_for(&auth.login("secret").unwrap());
        assert!(persistent(&path, "secret").is_err());
        // A rotated token must not bypass the lock and overwrite the store either.
        assert!(persistent(&path, "rotated").is_err());
        assert!(auth.authorized(&headers));
        drop(auth);
        let auth = persistent(&path, "secret").unwrap();
        assert!(auth.authorized(&headers));
        auth.logout(&headers).unwrap();
        assert!(persistent(&path, "secret").is_err());
        drop(auth);
        assert!(!persistent(&path, "secret").unwrap().authorized(&headers));
        let lock = dir.path().join("sessions.lock");
        assert_eq!(
            std::fs::metadata(lock).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn unsafe_lock_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions");
        let lock = dir.path().join("sessions.lock");
        persistent(&path, "secret").unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(persistent(&path, "secret").is_err());
        std::fs::remove_file(&lock).unwrap();
        std::os::unix::fs::symlink(dir.path().join("missing"), &lock).unwrap();
        assert!(persistent(&path, "secret").is_err());
        std::fs::remove_file(&lock).unwrap();
        std::fs::create_dir(&lock).unwrap();
        assert!(persistent(&path, "secret").is_err());
        std::fs::remove_dir(&lock).unwrap();
        std::fs::hard_link(&path, &lock).unwrap();
        assert!(persistent(&path, "secret").is_err());
    }

    #[test]
    fn persistence_logout_and_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.sessions");
        let auth = persistent(&path, "secret").unwrap();
        let cookie = auth.login("secret").unwrap();
        let headers = headers_for(&cookie);
        let raw = cookie.split(';').next().unwrap().split_once('=').unwrap().1;
        let disk = std::fs::read_to_string(&path).unwrap();
        assert!(!disk.contains(raw));
        assert!(!disk.contains("secret"));
        drop(auth);
        let auth = persistent(&path, "secret").unwrap();
        assert!(auth.authorized(&headers));
        auth.logout(&headers).unwrap();
        drop(auth);
        let auth = persistent(&path, "secret").unwrap();
        assert!(!auth.authorized(&headers));
        let headers = headers_for(&auth.login("secret").unwrap());
        drop(auth);
        assert!(!persistent(&path, "rotated").unwrap().authorized(&headers));
        assert!(!persistent(&path, "secret").unwrap().authorized(&headers));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn expiry_and_invalid_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions");
        let auth = persistent(&path, "secret").unwrap();
        let headers = headers_for(&auth.login("secret").unwrap());
        let id = hash(&cookie(&headers).unwrap());
        drop(auth);
        let mut data: SessionFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        data.sessions
            .insert(id.clone(), now().unwrap() - SESSION_LIFETIME);
        save_file(&path, &data).unwrap();
        assert!(!persistent(&path, "secret").unwrap().authorized(&headers));
        for invalid in [0, u64::MAX] {
            data.sessions.insert(id.clone(), invalid);
            save_file(&path, &data).unwrap();
            assert!(persistent(&path, "secret").is_err());
        }
    }

    #[test]
    fn unsafe_or_corrupt_store_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions");
        persistent(&path, "secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(persistent(&path, "secret").is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        assert!(persistent(&path, "secret").is_err());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(dir.path().join("missing"), &path).unwrap();
        assert!(persistent(&path, "secret").is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(persistent(&path, "secret").is_err());
    }

    #[test]
    fn failed_writes_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions");
        let auth = persistent(&path, "secret").unwrap();
        let headers = headers_for(&auth.login("secret").unwrap());
        // A directory at the destination reliably fails even under root.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(auth.login("secret").is_err());
        assert!(!auth.authorized(&headers));
        assert!(auth.sessions.lock().unwrap().entries.is_empty());
        assert!(auth.logout(&headers).is_err());
        assert!(persistent(&path, "secret").is_err());
    }

    #[test]
    fn failed_logout_does_not_report_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions");
        let auth = persistent(&path, "secret").unwrap();
        let headers = headers_for(&auth.login("secret").unwrap());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(auth.logout(&headers).is_err());
        assert!(!auth.authorized(&headers));
    }

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
        a.logout(&h).unwrap();
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
