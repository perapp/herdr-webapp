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

// Only used to reject already-expired sessions when migrating version 1 stores.
const LEGACY_SESSION_LIFETIME: u64 = 60 * 60 * 24 * 7;
// Chromium caps persistent cookies at 400 days. Server sessions never expire;
// browsers can still evict cookies (including earlier privacy-policy caps).
const COOKIE_MAX_AGE: u64 = 60 * 60 * 24 * 400;
const MAX_SESSIONS: usize = 1000;

#[derive(Default)]
struct Sessions {
    token: String,
    entries: HashMap<String, u64>,
    failed: bool,
    revision: u64,
    authenticated: bool,
}

/// Secret-free state for stream revocation and trusted terminal-display updates.
/// On every change, streams must recheck their original credentials with authorized().
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthState {
    pub revision: u64,
    /// True after the first successful authentication observed by this process.
    pub authenticated: bool,
}

/// Only for trusted local display code. Never serialize, log, or expose via HTTP.
pub struct CurrentTokenSnapshot {
    pub token: String,
    pub revision: u64,
    pub authenticated: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RotationJournal {
    version: u32,
    token: String,
    state: SessionFile,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionFile {
    version: u32,
    token_hash: String,
    sessions: HashMap<String, u64>,
}

// Deployed readers reject unknown canonical fields, so policy lives separately.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LifetimePolicy {
    version: u32,
    token_hash: String,
    sessions: HashMap<String, u64>,
}

fn lifetime_path(store: &Path) -> PathBuf {
    let mut path = store.as_os_str().to_os_string();
    path.push(".lifetime");
    path.into()
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn read_lifetime(store: &Path, current: u64) -> anyhow::Result<Option<LifetimePolicy>> {
    let file = match private_file(&lifetime_path(store)) {
        Ok(file) => file,
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(_) => bail!("cannot open private session lifetime policy"),
    };
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("cannot read private session lifetime policy"))?;
    if bytes.len() > 1024 * 1024 {
        bail!("session lifetime policy is too large");
    }
    let policy: LifetimePolicy = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid session lifetime policy"))?;
    if policy.version != 1
        || !valid_hash(&policy.token_hash)
        || policy.sessions.len() > MAX_SESSIONS
        || policy
            .sessions
            .iter()
            .any(|(id, created)| !valid_hash(id) || !valid_time(*created, current))
    {
        bail!("invalid session lifetime policy");
    }
    Ok(Some(policy))
}

fn save_lifetime(store: &Path, data: &SessionFile) -> anyhow::Result<()> {
    atomic_private_write(
        &lifetime_path(store),
        &serde_json::to_vec(&LifetimePolicy {
            version: 1,
            token_hash: data.token_hash.clone(),
            sessions: data.sessions.clone(),
        })?,
    )
}

// Policy first: interrupted saves cannot grant cookies absent from the main
// store. Legacy rewrites retain permanence only for matching IDs AND timestamps.
fn save_compatible(store: &Path, data: &SessionFile) -> anyhow::Result<()> {
    save_lifetime(store, data)?;
    save_file(
        store,
        &SessionFile {
            version: 1,
            token_hash: data.token_hash.clone(),
            sessions: data.sessions.clone(),
        },
    )
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
    created != 0 && created <= now
}

fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
    atomic_private_write(path, &serde_json::to_vec(data)?)
}

fn atomic_private_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
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
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        directory.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

fn journal_path(store: &Path) -> PathBuf {
    let mut path = store.as_os_str().to_os_string();
    path.push(".rotation");
    path.into()
}

fn read_token(path: &Path) -> anyhow::Result<String> {
    let mut bytes = Vec::new();
    private_file(path)?.take(4097).read_to_end(&mut bytes)?;
    let token = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid private token"))?
        .trim();
    if !(32..=4096).contains(&token.len())
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("invalid private token");
    }
    Ok(token.to_owned())
}

fn remove_journal(path: &Path) -> anyhow::Result<()> {
    std::fs::remove_file(path)?;
    File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    Ok(())
}

fn commit_rotation(
    store: &Path,
    token_path: &Path,
    token: &str,
    state: SessionFile,
) -> anyhow::Result<()> {
    // Refuse unsafe destinations before installing a durable commit intent.
    private_file(store)?;
    private_file(token_path)?;
    let journal = RotationJournal {
        version: 1,
        token: token.to_owned(),
        state,
    };
    let path = journal_path(store);
    // Journal contains the secret just like the token file, not the hashed
    // session store. It is private, bounded, atomically installed and fsynced.
    // Its durable presence is a redo-only commit record: recovery never restores
    // the old credentials, even if the process dies between the two renames.
    atomic_private_write(&path, &serde_json::to_vec(&journal)?)?;
    atomic_private_write(token_path, journal.token.as_bytes())?;
    save_compatible(store, &journal.state)?;
    remove_journal(&path)
}

fn recover_rotation(store: &Path, token_path: &Path) -> anyhow::Result<Option<String>> {
    let path = journal_path(store);
    let file = match private_file(&path) {
        Ok(file) => file,
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(_) => bail!("cannot open private rotation journal"),
    };
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        bail!("rotation journal is too large");
    }
    let journal: RotationJournal =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid rotation journal"))?;
    let current = now()?;
    if journal.version != 1
        || journal.token.len() != 64
        || !journal
            .token
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || !matches!(journal.state.version, 1 | 2)
        || !equal(&journal.state.token_hash, &hash(&journal.token))
        || journal.state.sessions.len() != 1
        || journal.state.sessions.iter().any(|(id, created)| {
            id.len() != 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                || !valid_time(*created, current)
        })
    {
        bail!("invalid rotation journal");
    }
    atomic_private_write(token_path, journal.token.as_bytes())?;
    save_compatible(store, &journal.state)?;
    remove_journal(&path)?;
    Ok(Some(journal.token))
}

pub struct Auth {
    sessions: Mutex<Sessions>,
    store: Option<PathBuf>,
    token_path: Option<PathBuf>,
    changes: tokio::sync::watch::Sender<AuthState>,
    authorization_gate: tokio::sync::RwLock<()>,
    _store_lock: Option<File>,
    /// Failed sign-ins in the last minute, keyed by client address bucket.
    attempts: Mutex<Vec<(Option<std::net::IpAddr>, Instant)>>,
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
    /// In-memory authentication for tests and private in-process adapters; never persisted.
    pub fn new(token: String, origin: Option<String>) -> Self {
        Self {
            sessions: Mutex::new(Sessions {
                token,
                ..Sessions::default()
            }),
            store: None,
            token_path: None,
            changes: tokio::sync::watch::channel(AuthState::default()).0,
            authorization_gate: tokio::sync::RwLock::new(()),
            _store_lock: None,
            attempts: Mutex::new(Vec::new()),
            origin,
        }
    }
    #[cfg(test)]
    pub fn persistent(
        token: String,
        origin: Option<String>,
        path: PathBuf,
    ) -> anyhow::Result<Self> {
        Self::open(token, origin, path, None)
    }

    /// Production LAN constructor: rotation updates this exact startup token file.
    /// Any durable interrupted transaction is completed while holding the stable
    /// session-store lock, before authenticating anyone or publishing the token.
    pub fn persistent_with_token_path(
        token: String,
        origin: Option<String>,
        path: PathBuf,
        token_path: PathBuf,
    ) -> anyhow::Result<Self> {
        Self::open(token, origin, path, Some(token_path))
    }

    fn open(
        mut token: String,
        origin: Option<String>,
        path: PathBuf,
        token_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let store_lock = lock_store(&path).context("lock private session store")?;
        if let Some(token_path) = &token_path {
            if let Some(recovered) = recover_rotation(&path, token_path)? {
                token = recovered;
            } else {
                // Re-read under the lock: the caller may have read before another
                // server finished rotating and released its lock.
                token = read_token(token_path)?;
            }
        }
        let current = now().context("read session clock")?;
        let policy = read_lifetime(&path, current)?;
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
                if !matches!(data.version, 1 | 2) || data.sessions.len() > MAX_SESSIONS {
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
                        let permanent = policy.as_ref().is_some_and(|policy| {
                            equal(&policy.token_hash, &data.token_hash)
                                && policy.sessions.get(&id) == Some(&created)
                        });
                        if data.version == 2
                            || permanent
                            || current - created < LEGACY_SESSION_LIFETIME
                        {
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
            origin,
            store: Some(path),
            token_path,
            changes: tokio::sync::watch::channel(AuthState::default()).0,
            authorization_gate: tokio::sync::RwLock::new(()),
            _store_lock: Some(store_lock),
            sessions: Mutex::new(Sessions {
                token,
                entries,
                ..Sessions::default()
            }),
            attempts: Mutex::new(Vec::new()),
        };
        auth.save(&auth.sessions.lock().unwrap())
            .context("persist private session store")?;
        Ok(auth)
    }

    fn save(&self, state: &Sessions) -> anyhow::Result<()> {
        if let Some(path) = &self.store {
            save_compatible(
                path,
                &SessionFile {
                    version: 1,
                    token_hash: hash(&state.token),
                    sessions: state.entries.clone(),
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

    /// Subscribe before checking credentials to avoid missing a revocation.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<AuthState> {
        self.changes.subscribe()
    }

    /// Trusted local terminal-display API. No Debug/Serialize implementation.
    pub fn current_token_snapshot(&self) -> Result<CurrentTokenSnapshot, ApiError> {
        let state = self.sessions.lock().unwrap();
        if state.failed {
            return Err(Self::storage_error());
        }
        Ok(CurrentTokenSnapshot {
            token: state.token.clone(),
            revision: state.revision,
            authenticated: state.authenticated,
        })
    }

    fn changed(&self, state: &mut Sessions) {
        state.revision += 1;
        self.changes.send_replace(AuthState {
            revision: state.revision,
            authenticated: state.authenticated,
        });
    }

    fn fail_closed(&self, state: &mut Sessions) {
        state.failed = true;
        state.entries.clear();
        self.changed(state);
    }

    fn authorized_locked(state: &Sessions, headers: &HeaderMap) -> bool {
        if state.failed {
            return false;
        }
        if let Some(v) = headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
        {
            return equal(&state.token, v);
        }
        Self::cookie_owner_locked(state, headers).is_some()
    }

    fn cookie_owner_locked(state: &Sessions, headers: &HeaderMap) -> Option<String> {
        if state.failed {
            return None;
        }
        let id = hash(&cookie(headers)?);
        state.entries.get(&id).and_then(|created| {
            now()
                .ok()
                .filter(|current| valid_time(*created, *current))
                .map(|_| id)
        })
    }

    /// Stable, non-secret push-owner key, shared by all tabs in this profile.
    /// Notifications must use this instead of a raw session cookie or access token.
    #[cfg(test)]
    pub fn cookie_owner(&self, headers: &HeaderMap) -> Option<String> {
        Self::cookie_owner_locked(&self.sessions.lock().unwrap(), headers)
    }

    /// Acquire before authenticating a notification mutation; retain through
    /// owner derivation, granting and persistence. Recheck after acquiring this
    /// gate: a request queued behind rotation may already have been revoked.
    /// Tokio guards are Send; the internal std mutex never crosses an await.
    pub async fn authorization_read(&self) -> tokio::sync::RwLockReadGuard<'_, ()> {
        self.authorization_gate.read().await
    }

    /// Rotation holds this across Auth::rotate and notification keep_owner.
    /// Existing notification requests finish first; queued ones must recheck
    /// the new auth state before they can grant/enroll an owner.
    pub async fn authorization_write(&self) -> tokio::sync::RwLockWriteGuard<'_, ()> {
        self.authorization_gate.write().await
    }

    /// Non-secret LAN owner mapping. Cookie owners match rotate()'s retained
    /// owner exactly. Bearer owners are token hashes, revoked by token rotation.
    /// For notification enrollment, call only AFTER acquiring authorization_read
    /// and hold that guard through the entire enrollment mutation.
    pub fn notification_owner(&self, headers: &HeaderMap) -> Result<String, ApiError> {
        let state = self.sessions.lock().unwrap();
        let denied = || error(StatusCode::UNAUTHORIZED, "Authentication required");
        if !Self::authorized_locked(&state, headers) {
            return Err(denied());
        }
        // An authenticated browser carrying both valid credentials remains
        // its cookie owner, matching the identity retained by rotation.
        if let Some(owner) = Self::cookie_owner_locked(&state, headers) {
            return Ok(owner);
        }
        if let Some(token) = headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .filter(|token| equal(&state.token, token))
        {
            return Ok(format!("bearer:{}", hash(token)));
        }
        Err(denied())
    }

    /// Authoritative startup whitelist; reconcile persisted push subscriptions
    /// before observing Core so failed cleanup cannot resurrect revoked owners.
    pub fn notification_owners(&self) -> Result<Vec<String>, ApiError> {
        let state = self.sessions.lock().unwrap();
        if state.failed {
            return Err(Self::storage_error());
        }
        let current = now().map_err(|_| Self::storage_error())?;
        let mut owners: Vec<String> = state
            .entries
            .iter()
            .filter(|(_, created)| valid_time(**created, current))
            .map(|(owner, _)| owner.clone())
            .collect();
        owners.push(format!("bearer:{}", hash(&state.token)));
        Ok(owners)
    }

    pub fn authorized(&self, headers: &HeaderMap) -> bool {
        let mut state = self.sessions.lock().unwrap();
        let authorized = Self::authorized_locked(&state, headers);
        if authorized && !state.authenticated {
            state.authenticated = true;
            self.changed(&mut state);
        }
        authorized
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
    #[cfg(test)]
    pub fn login(&self, token: &str) -> Result<String, ApiError> {
        self.login_from(None, token)
    }
    /// Only failed attempts count, per client address, so one LAN client
    /// guessing tokens cannot lock out everyone else. The global ceiling bounds
    /// memory and total guesses when many addresses take part.
    pub fn login_from(
        &self,
        from: Option<std::net::IpAddr>,
        token: &str,
    ) -> Result<String, ApiError> {
        let from = from.map(address_bucket);
        let mut attempts = self.attempts.lock().unwrap();
        attempts.retain(|(_, t)| t.elapsed() < Duration::from_secs(60));
        if attempts.len() >= MAX_FAILED_LOGINS
            || attempts.iter().filter(|(a, _)| *a == from).count() >= MAX_FAILED_LOGINS_PER_ADDRESS
        {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many failed sign-in attempts; try again in one minute",
            ));
        }
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.failed {
            return Err(Self::storage_error());
        }
        // Check inside the same lock as persistence; a queued stale login must
        // never create a new session after rotation commits.
        if !equal(&sessions.token, token) {
            attempts.push((from, Instant::now()));
            return Err(error(StatusCode::UNAUTHORIZED, "Invalid access token"));
        }
        let session = random_secret();
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
        let previous = std::mem::replace(&mut sessions.entries, next);
        if self.save(&sessions).is_err() {
            self.fail_closed(&mut sessions);
            return Err(Self::storage_error());
        }
        drop(previous);
        sessions.authenticated = true;
        self.changed(&mut sessions);
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
        sessions.entries = next;
        if self.save(&sessions).is_err() {
            self.fail_closed(&mut sessions);
            return Err(Self::storage_error());
        }
        self.changed(&mut sessions);
        Ok(self.cookie("", true))
    }
    /// Rotate while retaining the initiating cookie byte-for-byte. This preserves
    /// its existing WebSockets and shared-profile tabs, but revokes every other
    /// cookie and the old bearer token. Never returns a token to an HTTP caller.
    /// The returned value is the non-secret retained notification owner key.
    pub fn rotate(&self, headers: &HeaderMap) -> Result<String, ApiError> {
        // Deliberately do not use the bearer-only missing-Origin exception.
        self.same_origin(headers, true)?;
        let mut state = self.sessions.lock().unwrap();
        let owner = Self::cookie_owner_locked(&state, headers).ok_or_else(|| {
            error(
                StatusCode::UNAUTHORIZED,
                "A signed-in browser session is required",
            )
        })?;
        if !Self::authorized_locked(&state, headers) {
            return Err(error(StatusCode::UNAUTHORIZED, "Authentication required"));
        }
        let token = random_secret();
        let next = HashMap::from([(owner.clone(), state.entries[&owner])]);
        let saved = SessionFile {
            version: 1,
            token_hash: hash(&token),
            sessions: next.clone(),
        };
        let result = match (&self.store, &self.token_path) {
            (Some(store), Some(token_path)) => commit_rotation(store, token_path, &token, saved),
            (None, None) => Ok(()), // isolated in-memory adapters/tests
            _ => {
                return Err(error(
                    StatusCode::NOT_IMPLEMENTED,
                    "Token rotation is not configured",
                ));
            }
        };
        if result.is_err() {
            self.fail_closed(&mut state);
            return Err(Self::storage_error());
        }
        // Publish only after both private files and their directory entries are
        // durable. All authorization checks and other mutations use this lock.
        state.token = token;
        state.entries = next;
        state.authenticated = true;
        self.changed(&mut state);
        Ok(owner)
    }

    /// Refresh browser persistence without minting/changing a session identity.
    /// Used by /api/session to extend old seven-day cookies after migration.
    /// Browser privacy policies can still cap/evict cookies independently.
    /// A bearer-only request never receives a browser session cookie.
    pub fn renew_cookie(&self, headers: &HeaderMap) -> Option<String> {
        let state = self.sessions.lock().unwrap();
        if !Self::authorized_locked(&state, headers)
            || Self::cookie_owner_locked(&state, headers).is_none()
        {
            return None;
        }
        Some(self.cookie(&cookie(headers)?, false))
    }

    fn cookie(&self, value: &str, expired: bool) -> String {
        format!(
            "herdr_session={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
            if expired { 0 } else { COOKIE_MAX_AGE },
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

const MAX_FAILED_LOGINS_PER_ADDRESS: usize = 30;
const MAX_FAILED_LOGINS: usize = 600;

/// One IPv6 host normally controls a whole /64, so count that as one client.
fn address_bucket(address: std::net::IpAddr) -> std::net::IpAddr {
    match address.to_canonical() {
        std::net::IpAddr::V6(v6) => {
            let mut s = v6.segments();
            s[4..].fill(0);
            std::net::IpAddr::V6(s.into())
        }
        v4 => v4,
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
    // Only the asset handler sets its own policy (content-hashed files).
    h.entry(header::CACHE_CONTROL)
        .or_insert(axum::http::HeaderValue::from_static("no-store"));
    response.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_logins_are_limited_per_address_and_never_block_others() {
        let auth = Auth::new("secret".into(), None);
        let noisy = Some("192.168.1.50".parse().unwrap());
        let quiet = Some("192.168.1.51".parse().unwrap());
        for _ in 0..MAX_FAILED_LOGINS_PER_ADDRESS {
            assert_eq!(
                auth.login_from(noisy, "wrong").unwrap_err().0,
                StatusCode::UNAUTHORIZED
            );
        }
        // Even the right token is refused from the limited address.
        assert_eq!(
            auth.login_from(noisy, "secret").unwrap_err().0,
            StatusCode::TOO_MANY_REQUESTS
        );
        auth.login_from(quiet, "secret").unwrap();
        // Successful sign-ins never count against an address.
        for _ in 0..3 {
            auth.login_from(quiet, "secret").unwrap();
        }
        assert_eq!(
            auth.attempts.lock().unwrap().len(),
            MAX_FAILED_LOGINS_PER_ADDRESS
        );
        // One IPv6 /64 is one client.
        let a: std::net::IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: std::net::IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        assert_eq!(address_bucket(a), address_bucket(b));
        assert_ne!(
            address_bucket(a),
            address_bucket("2001:db8:1:3::1".parse().unwrap())
        );
        assert_eq!(
            address_bucket("::ffff:10.0.0.1".parse().unwrap()),
            "10.0.0.1".parse::<std::net::IpAddr>().unwrap()
        );
    }
    fn headers_for(cookie: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("cookie", cookie.split(';').next().unwrap().parse().unwrap());
        h
    }

    fn persistent(path: &Path, token: &str) -> anyhow::Result<Auth> {
        Auth::persistent(token.into(), None, path.into())
    }

    #[test]
    fn notification_owners_follow_authoritative_credentials_and_rotation() {
        let auth = Auth::new(
            "synthetic-token".into(),
            Some("https://synthetic.invalid".into()),
        );
        let mut keeper = headers_for(&auth.login("synthetic-token").unwrap());
        let other = headers_for(&auth.login("synthetic-token").unwrap());
        let owner = auth.notification_owner(&keeper).unwrap();
        let other_owner = auth.notification_owner(&other).unwrap();
        keeper.insert(
            header::AUTHORIZATION,
            "Bearer synthetic-token".parse().unwrap(),
        );
        assert_eq!(auth.notification_owner(&keeper).unwrap(), owner);
        let owners = auth.notification_owners().unwrap();
        assert!(owners.contains(&owner) && owners.contains(&other_owner));
        let old_bearer = format!("bearer:{}", hash("synthetic-token"));
        assert!(owners.contains(&old_bearer));
        assert!(!owners.iter().any(|owner| owner == "local"));
        keeper.insert(header::ORIGIN, "https://synthetic.invalid".parse().unwrap());
        assert_eq!(auth.rotate(&keeper).unwrap(), owner);
        let owners = auth.notification_owners().unwrap();
        assert_eq!(owners.len(), 2);
        assert!(owners.contains(&owner));
        assert!(!owners.contains(&other_owner) && !owners.contains(&old_bearer));
        assert!(!auth.authorized(&other));
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
        data.version = 1;
        std::fs::remove_file(lifetime_path(&path)).unwrap();
        data.sessions
            .insert(id.clone(), now().unwrap() - LEGACY_SESSION_LIFETIME);
        save_file(&path, &data).unwrap();
        assert!(!persistent(&path, "secret").unwrap().authorized(&headers));
        for invalid in [0, u64::MAX] {
            data.sessions.insert(id.clone(), invalid);
            save_file(&path, &data).unwrap();
            assert!(persistent(&path, "secret").is_err());
        }
    }

    #[test]
    fn sessions_survive_past_seven_days_and_restart_without_reviving_legacy_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions");
        let auth = persistent(&path, "secret").unwrap();
        let headers = headers_for(&auth.login("secret").unwrap());
        let id = hash(&cookie(&headers).unwrap());
        drop(auth);
        let mut data: SessionFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        data.sessions
            .insert(id.clone(), now().unwrap() - LEGACY_SESSION_LIFETIME * 5);
        save_compatible(&path, &data).unwrap();
        assert!(persistent(&path, "secret").unwrap().authorized(&headers));
        data.version = 1;
        std::fs::remove_file(lifetime_path(&path)).unwrap();
        save_file(&path, &data).unwrap();
        assert!(!persistent(&path, "secret").unwrap().authorized(&headers));
        // Migration persisted canonical v1 with the expired credential removed.
        assert!(!persistent(&path, "secret").unwrap().authorized(&headers));
    }

    fn rotating_auth(dir: &Path) -> Auth {
        let token_path = dir.join("token");
        if !token_path.exists() {
            atomic_private_write(&token_path, b"0123456789abcdef0123456789abcdef").unwrap();
        }
        Auth::persistent_with_token_path(
            read_token(&token_path).unwrap(),
            None,
            dir.join("sessions"),
            token_path,
        )
        .unwrap()
    }
    fn origin_headers(cookie: &str) -> HeaderMap {
        let mut headers = headers_for(cookie);
        headers.insert("host", "server:8001".parse().unwrap());
        headers.insert("origin", "http://server:8001".parse().unwrap());
        headers
    }

    #[test]
    fn rotation_is_durable_preserves_current_identity_and_revokes_cookie_and_bearer() {
        let dir = tempfile::tempdir().unwrap();
        let auth = rotating_auth(dir.path());
        let old = auth.current_token_snapshot().unwrap().token;
        let current = origin_headers(&auth.login(&old).unwrap());
        let other = origin_headers(&auth.login(&old).unwrap());
        let mut bearer = HeaderMap::new();
        bearer.insert("authorization", format!("Bearer {old}").parse().unwrap());
        let events = auth.subscribe();
        let original_owner = auth.cookie_owner(&current).unwrap();
        let owner = auth.rotate(&current).unwrap();
        assert_eq!(owner, original_owner);
        assert!(events.has_changed().unwrap());
        assert!(auth.authorized(&current));
        assert!(!auth.authorized(&other));
        assert!(!auth.authorized(&bearer));
        assert!(auth.login(&old).is_err());
        let new = auth.current_token_snapshot().unwrap().token;
        assert_ne!(old, new);
        assert_eq!(read_token(&dir.path().join("token")).unwrap(), new);
        assert!(
            !std::fs::read_to_string(dir.path().join("sessions"))
                .unwrap()
                .contains(&new)
        );
        drop(auth);
        let auth = rotating_auth(dir.path());
        assert_eq!(auth.current_token_snapshot().unwrap().token, new);
        assert!(auth.authorized(&current));
        assert!(!auth.authorized(&other));
        assert!(!auth.authorized(&bearer));
    }

    #[test]
    fn canonical_store_is_exact_legacy_schema_before_and_after_rotation() {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LegacySessionFile {
            version: u32,
            token_hash: String,
            sessions: HashMap<String, u64>,
        }
        let dir = tempfile::tempdir().unwrap();
        let auth = rotating_auth(dir.path());
        let old = auth.current_token_snapshot().unwrap().token;
        let keeper = origin_headers(&auth.login(&old).unwrap());
        let other = origin_headers(&auth.login(&old).unwrap());
        let store = dir.path().join("sessions");
        let before: LegacySessionFile =
            serde_json::from_slice(&std::fs::read(&store).unwrap()).unwrap();
        assert_eq!(before.version, 1);
        assert_eq!(before.token_hash, hash(&old));
        assert_eq!(before.sessions.len(), 2);
        let owner = auth.rotate(&keeper).unwrap();
        let after: LegacySessionFile =
            serde_json::from_slice(&std::fs::read(&store).unwrap()).unwrap();
        assert_eq!(after.version, 1);
        assert_eq!(
            after.token_hash,
            hash(&read_token(&dir.path().join("token")).unwrap())
        );
        assert_ne!(after.token_hash, before.token_hash);
        assert_eq!(after.sessions.len(), 1);
        assert_eq!(after.sessions.get(&owner), before.sessions.get(&owner));
        assert!(!after.sessions.contains_key(&hash(&cookie(&other).unwrap())));
    }

    #[test]
    fn legacy_rewrites_intersect_policy_and_never_revive_expired_unmarked_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions");
        let auth = persistent(&path, "secret").unwrap();
        let keeper = headers_for(&auth.login("secret").unwrap());
        let removed = headers_for(&auth.login("secret").unwrap());
        drop(auth);
        let mut data: SessionFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let keeper_id = hash(&cookie(&keeper).unwrap());
        data.sessions.insert(
            keeper_id.clone(),
            now().unwrap() - LEGACY_SESSION_LIFETIME * 5,
        );
        save_compatible(&path, &data).unwrap();
        // Old software removes one cookie and adds unmarked sessions; the stale
        // sidecar is not authoritative. Reusing an ID at a new time is unmarked.
        data.sessions.remove(&hash(&cookie(&removed).unwrap()));
        data.sessions
            .insert(hash("expired"), now().unwrap() - LEGACY_SESSION_LIFETIME);
        data.sessions.insert(hash("fresh"), now().unwrap());
        save_file(&path, &data).unwrap();
        let auth = persistent(&path, "secret").unwrap();
        assert!(auth.authorized(&keeper));
        assert!(!auth.authorized(&removed));
        assert!(!auth.authorized(&headers_for("herdr_session=expired")));
        assert!(auth.authorized(&headers_for("herdr_session=fresh")));
        drop(auth);
        data.sessions
            .insert(keeper_id, now().unwrap() - LEGACY_SESSION_LIFETIME * 2);
        save_file(&path, &data).unwrap();
        assert!(!persistent(&path, "secret").unwrap().authorized(&keeper));
    }

    #[test]
    fn policy_token_mismatch_is_legacy_and_dev_v2_is_rewritten_as_v1() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions");
        let auth = persistent(&path, "secret").unwrap();
        let headers = headers_for(&auth.login("secret").unwrap());
        drop(auth);
        let mut data: SessionFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        data.sessions.insert(
            hash(&cookie(&headers).unwrap()),
            now().unwrap() - LEGACY_SESSION_LIFETIME * 5,
        );
        save_compatible(&path, &data).unwrap();
        let mismatched = SessionFile {
            version: 1,
            token_hash: hash("other"),
            sessions: data.sessions.clone(),
        };
        save_lifetime(&path, &mismatched).unwrap();
        assert!(!persistent(&path, "secret").unwrap().authorized(&headers));
        data.version = 2;
        save_file(&path, &data).unwrap();
        assert!(persistent(&path, "secret").unwrap().authorized(&headers));
        let canonical: SessionFile =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(canonical.version, 1);
        assert!(persistent(&path, "secret").unwrap().authorized(&headers));
    }

    #[test]
    fn invalid_or_unsafe_lifetime_policy_is_refused_without_leaking_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions");
        drop(persistent(&path, "secret").unwrap());
        let policy_path = lifetime_path(&path);
        for policy in [
            serde_json::json!({"version":2,"token_hash":hash("secret"),"sessions":{}}),
            serde_json::json!({"version":1,"token_hash":"sensitive-invalid","sessions":{}}),
            serde_json::json!({"version":1,"token_hash":hash("secret"),"sessions":{"sensitive-invalid":1}}),
            serde_json::json!({"version":1,"token_hash":hash("secret"),"sessions":{hash("cookie"):0}}),
            serde_json::json!({"version":1,"token_hash":hash("secret"),"sessions":{hash("cookie"):u64::MAX}}),
        ] {
            atomic_private_write(&policy_path, &serde_json::to_vec(&policy).unwrap()).unwrap();
            let err = persistent(&path, "secret").err().unwrap().to_string();
            assert!(!err.contains("sensitive-invalid"));
        }
        std::fs::remove_file(&policy_path).unwrap();
        std::os::unix::fs::symlink(dir.path().join("missing"), &policy_path).unwrap();
        assert!(persistent(&path, "secret").is_err());
    }

    #[test]
    fn rotation_requires_cookie_and_exact_origin() {
        let auth = Auth::new("secret".into(), None);
        let current = origin_headers(&auth.login("secret").unwrap());
        for name in ["origin", "cookie"] {
            let mut invalid = current.clone();
            invalid.remove(name);
            assert!(auth.rotate(&invalid).is_err());
        }
        let mut foreign = current.clone();
        foreign.insert("origin", "http://evil".parse().unwrap());
        assert!(auth.rotate(&foreign).is_err());
        let mut bearer = current.clone();
        bearer.remove("cookie");
        bearer.insert("authorization", "Bearer secret".parse().unwrap());
        assert!(auth.rotate(&bearer).is_err());
        assert_eq!(auth.current_token_snapshot().unwrap().token, "secret");
    }

    #[test]
    fn concurrent_rotations_recheck_queued_credentials_and_only_one_profile_survives() {
        let auth = std::sync::Arc::new(Auth::new("secret".into(), None));
        let first = origin_headers(&auth.login("secret").unwrap());
        let second = origin_headers(&auth.login("secret").unwrap());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = [first.clone(), second.clone()]
            .into_iter()
            .map(|headers| {
                let auth = auth.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    auth.rotate(&headers).is_ok()
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|ok| **ok).count(), 1);
        assert_ne!(auth.authorized(&first), auth.authorized(&second));
        assert!(auth.login("secret").is_err());
    }

    #[tokio::test]
    async fn queued_notification_enrollment_rechecks_owner_after_rotation() {
        let auth = std::sync::Arc::new(Auth::new("secret".into(), None));
        let current = origin_headers(&auth.login("secret").unwrap());
        let other = origin_headers(&auth.login("secret").unwrap());
        let mut bearer = HeaderMap::new();
        bearer.insert("authorization", "Bearer secret".parse().unwrap());
        assert!(
            auth.notification_owner(&bearer)
                .unwrap()
                .starts_with("bearer:")
        );
        assert_ne!(
            auth.notification_owner(&current).unwrap(),
            auth.notification_owner(&other).unwrap()
        );
        let guard = auth.authorization_write().await;
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let stale_auth = auth.clone();
        let stale = other.clone();
        let enrollment = tokio::spawn(async move {
            ready.send(()).unwrap();
            let _guard = stale_auth.authorization_read().await;
            stale_auth.notification_owner(&stale)
        });
        waiting.await.unwrap();
        let owner = auth.rotate(&current).unwrap();
        assert_eq!(auth.notification_owner(&current).unwrap(), owner);
        assert!(auth.notification_owner(&bearer).is_err());
        drop(guard);
        assert!(enrollment.await.unwrap().is_err());
    }

    #[test]
    fn rotation_disk_failure_fails_closed_and_does_not_publish() {
        let dir = tempfile::tempdir().unwrap();
        let auth = rotating_auth(dir.path());
        let old = auth.current_token_snapshot().unwrap().token;
        let current = origin_headers(&auth.login(&old).unwrap());
        // A malicious/invalid destination must never be overwritten.
        std::fs::remove_file(dir.path().join("token")).unwrap();
        std::fs::create_dir(dir.path().join("token")).unwrap();
        assert!(auth.rotate(&current).is_err());
        assert!(!auth.authorized(&current));
        assert!(auth.current_token_snapshot().is_err());
        assert!(auth.login(&old).is_err());
    }

    #[test]
    fn crash_recovery_redoes_committed_intent_at_each_file_boundary() {
        for token_written in [false, true] {
            for (policy_written, sessions_written) in [(false, false), (true, false), (true, true)]
            {
                let dir = tempfile::tempdir().unwrap();
                let auth = rotating_auth(dir.path());
                let old = auth.current_token_snapshot().unwrap().token;
                let current = origin_headers(&auth.login(&old).unwrap());
                let other = origin_headers(&auth.login(&old).unwrap());
                let owner = auth.cookie_owner(&current).unwrap();
                let token = random_secret();
                let data = SessionFile {
                    version: 1,
                    token_hash: hash(&token),
                    sessions: HashMap::from([(
                        owner,
                        now().unwrap() - LEGACY_SESSION_LIFETIME * 5,
                    )]),
                };
                let journal = RotationJournal {
                    version: 1,
                    token: token.clone(),
                    state: data,
                };
                let store = dir.path().join("sessions");
                atomic_private_write(
                    &journal_path(&store),
                    &serde_json::to_vec(&journal).unwrap(),
                )
                .unwrap();
                if token_written {
                    atomic_private_write(&dir.path().join("token"), token.as_bytes()).unwrap();
                }
                if policy_written {
                    save_lifetime(&store, &journal.state).unwrap();
                }
                if sessions_written {
                    save_file(&store, &journal.state).unwrap();
                }
                drop(auth);
                // Explicitly supply a stale pre-recovery startup token.
                let auth = Auth::persistent_with_token_path(
                    old.clone(),
                    None,
                    store.clone(),
                    dir.path().join("token"),
                )
                .unwrap();
                assert!(auth.authorized(&current));
                assert!(!auth.authorized(&other));
                assert!(auth.login(&old).is_err());
                assert_eq!(auth.current_token_snapshot().unwrap().token, token);
                assert_eq!(read_token(&dir.path().join("token")).unwrap(), token);
                assert!(!journal_path(&store).exists());
                drop(auth);
                assert!(rotating_auth(dir.path()).authorized(&current));
            }
        }
    }

    #[test]
    fn authentication_events_and_persistent_cookie_security() {
        let auth = Auth::new("secret".into(), Some("https://server".into()));
        let mut changes = auth.subscribe();
        assert!(!changes.borrow().authenticated);
        assert!(auth.login("wrong").is_err());
        assert!(!changes.has_changed().unwrap());
        let cookie = auth.login("secret").unwrap();
        assert!(changes.has_changed().unwrap());
        assert!(changes.borrow_and_update().authenticated);
        assert!(cookie.contains("HttpOnly; SameSite=Strict; Max-Age=34560000; Secure"));
        auth.logout(&headers_for(&cookie)).unwrap();
        assert!(changes.has_changed().unwrap());
    }

    #[test]
    fn cookie_renewal_preserves_identity_and_never_mints_bearer_sessions() {
        let auth = Auth::new("secret".into(), None);
        let original = auth.login("secret").unwrap();
        let headers = headers_for(&original);
        let renewed = auth.renew_cookie(&headers).unwrap();
        assert_eq!(renewed.split(';').next(), original.split(';').next());
        assert!(renewed.contains("Max-Age=34560000"));
        let mut bearer = HeaderMap::new();
        bearer.insert("authorization", "Bearer secret".parse().unwrap());
        assert!(auth.renew_cookie(&bearer).is_none());
        auth.logout(&headers).unwrap();
        assert!(auth.renew_cookie(&headers).is_none());
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
