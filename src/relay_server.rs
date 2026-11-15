//! Standalone hosted signaling: no Core connection and no local authentication bypass.
//! Public key enrollment is intentionally public, bounded by global/per-IP quotas;
//! possession of a registered key is not authorization to use a laptop or obtain TURN.
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    extract::{
        ConnectInfo, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};

const MAX_MESSAGE: usize = 2 * 1024 * 1024;
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const JOIN_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct RelayConfig {
    pub public_origin: String,
    pub base_path: String,
    pub revision: Option<String>,
    pub unix_socket: Option<std::path::PathBuf>,
    pub main: Option<crate::relay_main::MainConfig>,
    pub bind: IpAddr,
    pub port: u16,
    pub allow_insecure_loopback: bool,
    pub turn_urls: Vec<String>,
    pub turn_secret: Option<String>,
    pub turn_ttl_seconds: u64,
    pub max_rooms: usize,
    pub max_sessions: usize,
    pub max_connections: usize,
    pub max_connections_per_ip: usize,
}
impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            public_origin: String::new(),
            base_path: "/".into(),
            revision: None,
            unix_socket: None,
            main: None,
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 8080,
            allow_insecure_loopback: false,
            turn_urls: vec![],
            turn_secret: None,
            turn_ttl_seconds: 600,
            max_rooms: 64,
            max_sessions: 128,
            max_connections: 256,
            max_connections_per_ip: 32,
        }
    }
}
impl RelayConfig {
    fn context(&self) -> String {
        if self.base_path == "/" {
            self.public_origin.clone()
        } else {
            format!(
                "{}{}",
                self.public_origin,
                self.base_path.trim_end_matches('/')
            )
        }
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.base_path.as_str(), "/" | "/main/"),
            "unsupported channel base path"
        );
        ensure!(
            if self.base_path == "/main/" {
                self.main.is_none()
                    && self
                        .revision
                        .as_deref()
                        .is_some_and(crate::relay_main::valid_revision)
                    && self.unix_socket.is_some()
            } else {
                self.revision.is_none() && self.unix_socket.is_none()
            },
            "invalid internal main channel configuration"
        );
        // Restrict to ASCII origin syntax also safe inside the bootstrap shell literal.
        ensure!(
            !self.public_origin.is_empty()
                && self
                    .public_origin
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-.:/[]".contains(&b)),
            "invalid public origin"
        );
        let uri: Uri = self.public_origin.parse()?;
        let host = uri.host().unwrap_or_default();
        let loopback = host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        ensure!(
            uri.authority().is_some()
                && !host.is_empty()
                && uri.path() == "/"
                && uri.query().is_none()
                && !self.public_origin.ends_with('/'),
            "public origin must contain only scheme and authority"
        );
        ensure!(
            uri.scheme_str() == Some("https")
                || (uri.scheme_str() == Some("http")
                    && self.allow_insecure_loopback
                    && loopback
                    && (self.bind.is_loopback() || self.unix_socket.is_some())),
            "HTTPS required except explicit loopback development mode"
        );
        ensure!(
            (1..=64).contains(&self.max_rooms)
                && (1..=128).contains(&self.max_sessions)
                && (1..=512).contains(&self.max_connections)
                && (1..=64).contains(&self.max_connections_per_ip),
            "relay quota exceeds hard maximum or is zero"
        );
        ensure!(
            (30..=600).contains(&self.turn_ttl_seconds),
            "TURN lifetime must be 30..600 seconds"
        );
        ensure!(
            self.turn_urls.len() <= 8
                && self.turn_urls.iter().all(|u| u.len() <= 512
                    && (u.starts_with("turn:") || u.starts_with("turns:"))
                    && !u.chars().any(|c| c.is_control() || c.is_whitespace())),
            "invalid TURN URLs"
        );
        ensure!(
            self.turn_urls.is_empty() == self.turn_secret.is_none(),
            "TURN URLs and secret must be configured together"
        );
        ensure!(
            self.turn_secret
                .as_ref()
                .is_none_or(|s| s.len() >= 32 && s.len() <= 4096),
            "TURN secret must contain 32..4096 bytes"
        );
        Ok(())
    }
}

// Bound bytes as well as message count: 32 large SDP messages per connection
// must not multiply into unbounded gigabytes across the public service.
struct Outbound {
    message: Message,
    _budget: OwnedSemaphorePermit,
}
type Tx = mpsc::Sender<Outbound>;
struct Peer {
    tx: Tx,
    ip: IpAddr,
}
struct Room {
    owner: u64,
}
struct Session {
    machine: String,
    browser: u64,
    connector: u64,
    authorized: bool,
    expires: Instant,
}
struct Registry {
    next_id: u64,
    peers: HashMap<u64, Peer>,
    rooms: HashMap<String, Room>,
    sessions: HashMap<String, Session>,
    // Bounded per-IP upgrade bucket, including requests that do not register.
    admission: HashMap<IpAddr, Bucket>,
    queue_budget: Arc<Semaphore>,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            next_id: 0,
            peers: HashMap::new(),
            rooms: HashMap::new(),
            sessions: HashMap::new(),
            admission: HashMap::new(),
            queue_budget: Arc::new(Semaphore::new(64 * 1024 * 1024)),
        }
    }
}
struct Relay {
    config: RelayConfig,
    registry: Mutex<Registry>,
    shutdown: watch::Sender<bool>,
}
struct Bucket {
    tokens: f64,
    updated: Instant,
}
impl Bucket {
    fn new() -> Self {
        Self {
            tokens: 30.0,
            updated: Instant::now(),
        }
    }
    fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens =
            (self.tokens + now.duration_since(self.updated).as_secs_f64() * 30.0).min(30.0);
        self.updated = now;
        if self.tokens < 1.0 {
            false
        } else {
            self.tokens -= 1.0;
            true
        }
    }
}

/// Routes are independent of local/LAN Auth and Core. Merge the release catalog
/// before serving; the provided `run` does this and supplies real peer addresses.
#[cfg(test)]
pub fn router(config: RelayConfig) -> Result<Router> {
    config.validate()?;
    Ok(routes(Arc::new(Relay {
        config,
        registry: Mutex::new(Registry::default()),
        shutdown: watch::channel(false).0,
    })))
}
fn routes(state: Arc<Relay>) -> Router {
    Router::new()
        // A fallback alone does not create the empty-tail route under nest().
        .route("/", get(asset))
        .route("/relay/socket", get(upgrade))
        .route("/api/mode", get(mode))
        .route(
            "/healthz",
            get(|| async { Json(json!({"ready":true,"protocolVersion":1})) }),
        )
        .route("/run", get(bootstrap))
        .route("/client/launcher.py", get(launcher))
        .fallback(get(asset))
        .layer(middleware::from_fn(headers))
        .with_state(state)
}
pub async fn run(config: RelayConfig, catalog: Option<Router>) -> Result<()> {
    config.validate()?;
    let addr = SocketAddr::new(config.bind, config.port);
    let unix_socket = config.unix_socket.clone();
    // A failed public bind must not leave a build supervisor behind.
    let tcp_listener = if unix_socket.is_none() {
        Some(tokio::net::TcpListener::bind(addr).await?)
    } else {
        None
    };
    let main = config
        .main
        .clone()
        .map(crate::relay_main::MainService::start)
        .transpose()?;
    let base_path = config.base_path.clone();
    let state = Arc::new(Relay {
        config,
        registry: Mutex::new(Registry::default()),
        shutdown: watch::channel(false).0,
    });
    let app = routes(state.clone())
        .merge(catalog.unwrap_or_else(crate::relay_catalog::unavailable_router));
    let app = if base_path == "/main/" {
        Router::new().nest("/main/", app)
    } else {
        app.merge(crate::relay_main::router(main.clone()))
    };
    let shutdown = async move {
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("install SIGTERM handler");
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        let _ = state.shutdown.send(true);
        if let Some(main) = main {
            main.shutdown().await;
        }
    };
    if let Some(path) = unix_socket {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            path.as_os_str().len() <= 100,
            "internal socket path too long"
        );
        let listener = tokio::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        // The private endpoint has a shared admission bucket. Never trust public
        // forwarding headers as client identity. Only this UID can reach it.
        let app = app.layer(axum::Extension(ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            0,
        )))));
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await?;
        let _ = std::fs::remove_file(path);
    } else {
        let listener = tcp_listener.expect("public listener bound before starting supervisor");
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
        .await?;
    }
    Ok(())
}
async fn mode(State(s): State<Arc<Relay>>) -> Json<Value> {
    Json(
        json!({"mode":"relay","version":env!("CARGO_PKG_VERSION"),"protocolVersion":1,"entrypointProtocolVersion":1,"publicOrigin":s.config.public_origin,"channel":if s.config.base_path == "/" {"stable"} else {"main"},"basePath":s.config.base_path,"revision":s.config.revision,"buildRevision":crate::build_revision(),"build":crate::build_info::BuildInfo::current(),"launcherUrl":format!("{}run", s.config.base_path)}),
    )
}
async fn bootstrap(State(s): State<Arc<Relay>>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        include_str!("../distribution/run-relay.sh")
            .replace("__HERDR_RELAY_ORIGIN__", &s.config.context()),
    )
        .into_response()
}
async fn launcher() -> Response {
    (
        [(header::CONTENT_TYPE, "text/x-python; charset=utf-8")],
        include_str!("../distribution/relay-launcher.py"),
    )
        .into_response()
}
async fn asset(State(s): State<Arc<Relay>>, uri: Uri) -> Response {
    // Resolve only this child's configured prefix when present. Stable requests
    // must never use this normalization to fall through from reserved /main/.
    let raw = uri.path();
    let path = if s.config.base_path == "/main/" {
        raw.strip_prefix("/main/")
            .unwrap_or_else(|| raw.trim_start_matches('/'))
    } else {
        raw.trim_start_matches('/')
    };
    if path == "main" || path.starts_with("main/") || path.starts_with("main%") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if path.starts_with("api/") || path.starts_with("client/") || path.starts_with("relay/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(file) = crate::Assets::get(path) {
        let mut response = (
            [(
                header::CONTENT_TYPE,
                mime_guess::from_path(path).first_or_octet_stream().as_ref(),
            )],
            file.data.into_owned(),
        )
            .into_response();
        // Vite-generated, content-hashed assets only; all other content is no-store.
        if path.starts_with("assets/") && path.rsplit('/').next().is_some_and(|n| n.contains('-')) {
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            );
        }
        response
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
async fn headers(request: axum::extract::Request, next: Next) -> Response {
    let mut r = next.run(request).await;
    let h = r.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:; font-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    r
}
fn valid_origin(headers: &HeaderMap, origin: &str) -> bool {
    // Native clients may omit Origin, but only a verified registration can become
    // a connector. Any provided Origin must occur exactly once and match exactly.
    let values = headers.get_all(header::ORIGIN).iter().collect::<Vec<_>>();
    values.is_empty() || (values.len() == 1 && values[0].as_bytes() == origin.as_bytes())
}
async fn upgrade(
    State(s): State<Arc<Relay>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !valid_origin(&headers, &s.config.public_origin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let has_origin = headers.contains_key(header::ORIGIN);
    let (tx, rx) = mpsc::channel(32);
    let id = {
        let mut r = s.registry.lock().unwrap();
        r.admission
            .retain(|_, b| b.updated.elapsed() < Duration::from_secs(60));
        if (!r.admission.contains_key(&addr.ip()) && r.admission.len() >= 1024)
            || !r
                .admission
                .entry(addr.ip())
                .or_insert_with(Bucket::new)
                .take()
            || r.peers.len() >= s.config.max_connections
            || r.peers.values().filter(|p| p.ip == addr.ip()).count()
                >= s.config.max_connections_per_ip
        {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        r.next_id += 1;
        let id = r.next_id;
        r.peers.insert(id, Peer { tx, ip: addr.ip() });
        id
    };
    // Upgrade failure must release the reserved peer even when on_upgrade is never called.
    let fail_state = s.clone();
    ws.max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_failed_upgrade(move |_| {
            cleanup(&fail_state, id);
        })
        .on_upgrade(move |socket| connection(s, id, has_origin, socket, rx))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum Incoming {
    Register {
        #[serde(rename = "machineId")]
        machine_id: String,
        #[serde(rename = "publicKey")]
        public_key: String,
        version: String,
        nonce: String,
        signature: String,
    },
    Join {
        #[serde(rename = "machineId")]
        machine_id: String,
    },
    Signal {
        #[serde(rename = "sessionId")]
        session_id: String,
        payload: String,
    },
    Authorize {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
}
fn random_id() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
fn send(r: &Registry, id: u64, value: Value) -> bool {
    let text = value.to_string();
    let Ok(budget) = r
        .queue_budget
        .clone()
        .try_acquire_many_owned(text.len() as u32)
    else {
        return false;
    };
    r.peers.get(&id).is_some_and(|p| {
        p.tx.try_send(Outbound {
            message: Message::Text(text.into()),
            _budget: budget,
        })
        .is_ok()
    })
}
fn ended(r: &mut Registry, session_id: &str) {
    if let Some(s) = r.sessions.remove(session_id) {
        let event = json!({"type":"ended","sessionId":session_id});
        send(r, s.browser, event.clone());
        send(r, s.connector, event);
    }
}
fn cleanup(s: &Relay, id: u64) {
    let mut r = s.registry.lock().unwrap();
    r.peers.remove(&id);
    // Retain by connection identity, never erase a replacement registration.
    r.rooms.retain(|_, room| room.owner != id);
    let sessions: Vec<_> = r
        .sessions
        .iter()
        .filter(|(_, v)| v.browser == id || v.connector == id)
        .map(|(k, _)| k.clone())
        .collect();
    for sid in sessions {
        ended(&mut r, &sid);
    }
}
fn expire(s: &Relay) {
    let mut r = s.registry.lock().unwrap();
    let sessions: Vec<_> = r
        .sessions
        .iter()
        .filter(|(_, v)| !v.authorized && v.expires <= Instant::now())
        .map(|(k, _)| k.clone())
        .collect();
    for sid in sessions {
        ended(&mut r, &sid);
    }
}
#[derive(Default)]
enum Role {
    #[default]
    Fresh,
    Connector(String),
    Browser(String),
}

fn handle(
    s: &Relay,
    id: u64,
    has_origin: bool,
    nonce: &str,
    role: &mut Role,
    message: Incoming,
) -> Result<()> {
    match message {
        Incoming::Register {
            machine_id,
            public_key,
            version,
            nonce: received,
            signature,
        } => {
            ensure!(
                matches!(role, Role::Fresh)
                    && received == nonce
                    && version.len() <= 64
                    && !version.is_empty()
                    && version
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b)),
                "invalid message"
            );
            ensure!(
                public_key.len() <= 128 && signature.len() <= 128 && machine_id.len() == 43,
                "invalid message"
            );
            ensure!(
                crate::relay_crypto::machine_id(&public_key)? == machine_id,
                "invalid message"
            );
            let transcript = format!(
                "herdr-register-v1\n{}\n{machine_id}\n{nonce}\n{version}",
                s.config.context()
            );
            crate::relay_crypto::verify(&public_key, transcript.as_bytes(), &signature)?;
            let mut r = s.registry.lock().unwrap();
            ensure!(
                r.rooms.len() < s.config.max_rooms && !r.rooms.contains_key(&machine_id),
                "invalid message"
            );
            r.rooms.insert(machine_id.clone(), Room { owner: id });
            *role = Role::Connector(machine_id.clone());
            ensure!(
                send(
                    &r,
                    id,
                    json!({"type":"registered","machineId":machine_id,"protocolVersion":1})
                ),
                "backpressure"
            );
        }
        Incoming::Join { machine_id } => {
            // Browsers must supply exact Origin; an Origin-less native connection
            // is registration-only, not an anonymous browser-auth shortcut.
            ensure!(
                has_origin && matches!(role, Role::Fresh) && machine_id.len() == 43,
                "invalid message"
            );
            let mut r = s.registry.lock().unwrap();
            ensure!(r.sessions.len() < s.config.max_sessions, "invalid message");
            let connector = r
                .rooms
                .get(&machine_id)
                .map(|room| room.owner)
                .ok_or_else(|| anyhow::anyhow!("invalid message"))?;
            ensure!(
                r.sessions
                    .values()
                    .filter(|v| v.machine == machine_id && !v.authorized)
                    .count()
                    < 4,
                "invalid message"
            );
            let sid = random_id();
            r.sessions.insert(
                sid.clone(),
                Session {
                    machine: machine_id.clone(),
                    browser: id,
                    connector,
                    authorized: false,
                    expires: Instant::now() + JOIN_TIMEOUT,
                },
            );
            *role = Role::Browser(sid.clone());
            let event = json!({"type":"session","sessionId":sid,"machineId":machine_id});
            ensure!(
                send(&r, connector, event.clone()) && send(&r, id, event),
                "backpressure"
            );
        }
        Incoming::Signal {
            session_id,
            payload,
        } => {
            ensure!(
                session_id.len() == 43 && payload.len() <= MAX_MESSAGE - 256,
                "invalid message"
            );
            let r = s.registry.lock().unwrap();
            let session = r
                .sessions
                .get(&session_id)
                .filter(|v| v.authorized || v.expires > Instant::now())
                .ok_or_else(|| anyhow::anyhow!("invalid message"))?;
            let destination = match role {
                Role::Connector(machine)
                    if *machine == session.machine && id == session.connector =>
                {
                    session.browser
                }
                Role::Browser(sid) if *sid == session_id && id == session.browser => {
                    session.connector
                }
                _ => anyhow::bail!("invalid message"),
            };
            ensure!(
                send(
                    &r,
                    destination,
                    json!({"type":"signal","sessionId":session_id,"payload":payload})
                ),
                "backpressure"
            );
        }
        Incoming::Authorize { session_id } => {
            let mut r = s.registry.lock().unwrap();
            let session = r
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| anyhow::anyhow!("invalid message"))?;
            ensure!(
                matches!(role, Role::Connector(machine) if *machine == session.machine)
                    && session.connector == id
                    && !session.authorized
                    && session.expires > Instant::now(),
                "invalid message"
            );
            session.authorized = true;
            let browser = session.browser;
            let ice = ice(&s.config, &session_id)?;
            let expires_at =
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + s.config.turn_ttl_seconds;
            let event = json!({"type":"ice","sessionId":session_id,"iceServers":ice,"expiresAt":expires_at});
            ensure!(
                send(&r, browser, event.clone()) && send(&r, id, event),
                "backpressure"
            );
        }
    }
    Ok(())
}
fn ice(config: &RelayConfig, sid: &str) -> Result<Vec<Value>> {
    let Some(secret) = &config.turn_secret else {
        return Ok(vec![]);
    };
    let expires = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + config.turn_ttl_seconds;
    let username = format!(
        "{expires}:{}",
        URL_SAFE_NO_PAD.encode(Sha256::digest(sid.as_bytes()))
    );
    let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes())?;
    mac.update(username.as_bytes());
    Ok(vec![
        json!({"urls":config.turn_urls,"username":username,"credential":STANDARD.encode(mac.finalize().into_bytes())}),
    ])
}
async fn connection(
    s: Arc<Relay>,
    id: u64,
    has_origin: bool,
    socket: WebSocket,
    mut outgoing: mpsc::Receiver<Outbound>,
) {
    // RAII covers cancellation and every early return.
    struct Guard(Arc<Relay>, u64);
    impl Drop for Guard {
        fn drop(&mut self) {
            cleanup(&self.0, self.1);
        }
    }
    let _guard = Guard(s.clone(), id);
    let mut shutdown = s.shutdown.subscribe();
    let nonce = random_id();
    let (mut sink, mut source) = socket.split();
    if !matches!(
        tokio::time::timeout(
            SEND_TIMEOUT,
            sink.send(Message::Text(
                json!({"type":"challenge","nonce":nonce}).to_string().into()
            ))
        )
        .await,
        Ok(Ok(()))
    ) {
        return;
    }
    let mut role = Role::Fresh;
    let mut bucket = Bucket::new();
    let started = Instant::now();
    let mut last_seen = Instant::now();
    let mut ticks = tokio::time::interval(Duration::from_secs(5));
    let mut last_ping = Instant::now();
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticks.tick() => {
                expire(&s);
                if last_seen.elapsed() > Duration::from_secs(45) || (matches!(role, Role::Fresh) && started.elapsed() > JOIN_TIMEOUT) { break; }
                if last_ping.elapsed() >= Duration::from_secs(15) {
                    if !matches!(tokio::time::timeout(SEND_TIMEOUT, sink.send(Message::Ping(vec![].into()))).await, Ok(Ok(()))) { break; }
                    last_ping = Instant::now();
                }
            }
            message = outgoing.recv() => {
                let Some(message) = message else { break; };
                if !matches!(tokio::time::timeout(SEND_TIMEOUT, sink.send(message.message)).await, Ok(Ok(()))) { break; }
            }
            message = source.next() => {
                let Some(Ok(message)) = message else { break; };
                last_seen = Instant::now();
                if !bucket.take() { break; }
                match message {
                    Message::Text(text) => {
                        let result = serde_json::from_str::<Incoming>(&text).map_err(anyhow::Error::from).and_then(|m| handle(&s, id, has_origin, &nonce, &mut role, m));
                        if result.is_err() {
                            // All failures have identical, payload-free public diagnostics.
                            let _ = tokio::time::timeout(SEND_TIMEOUT, sink.send(Message::Text(json!({"type":"error","message":"invalid relay request"}).to_string().into()))).await;
                            break;
                        }
                    }
                    Message::Ping(payload) => {
                        if !matches!(tokio::time::timeout(SEND_TIMEOUT, sink.send(Message::Pong(payload))).await, Ok(Ok(()))) { break; }
                    }
                    Message::Pong(_) => {},
                    _ => break,
                }
            }
        }
    }
    let _ = tokio::time::timeout(SEND_TIMEOUT, sink.close()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    fn config() -> RelayConfig {
        RelayConfig {
            public_origin: "https://relay.example".into(),
            ..RelayConfig::default()
        }
    }
    fn state() -> Relay {
        Relay {
            config: config(),
            registry: Mutex::new(Registry::default()),
            shutdown: watch::channel(false).0,
        }
    }
    fn peer(s: &Relay, id: u64) -> mpsc::Receiver<Outbound> {
        let (tx, rx) = mpsc::channel(32);
        s.registry.lock().unwrap().peers.insert(
            id,
            Peer {
                tx,
                ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            },
        );
        rx
    }
    #[tokio::test]
    async fn mode_preserves_legacy_fields_and_adds_build() {
        let mut relay = state();
        relay.config.base_path = "/main/".into();
        relay.config.revision = Some("a".repeat(40));
        let Json(mode) = super::mode(State(Arc::new(relay))).await;
        assert_eq!(mode["mode"], "relay");
        assert_eq!(mode["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(mode["protocolVersion"], 1);
        assert_eq!(mode["entrypointProtocolVersion"], 1);
        assert_eq!(mode["channel"], "main");
        assert_eq!(mode["basePath"], "/main/");
        assert_eq!(mode["revision"], "a".repeat(40));
        assert_eq!(mode["buildRevision"], json!(crate::build_revision()));
        assert_eq!(mode["launcherUrl"], "/main/run");
        assert_eq!(
            mode["build"],
            serde_json::to_value(crate::build_info::BuildInfo::current()).unwrap()
        );
    }
    #[test]
    fn origins_and_config() {
        assert!(config().validate().is_ok());
        for origin in [
            "http://relay.example",
            "https://relay.example/",
            "https://relay.example/path",
            "https://relay.example?x",
            "https://x'bad",
            "https://user@host",
        ] {
            assert!(
                RelayConfig {
                    public_origin: origin.into(),
                    ..config()
                }
                .validate()
                .is_err(),
                "{origin}"
            );
        }
        assert!(
            RelayConfig {
                public_origin: "http://127.0.0.1:8080".into(),
                allow_insecure_loopback: true,
                ..config()
            }
            .validate()
            .is_ok()
        );
        let mut h = HeaderMap::new();
        assert!(valid_origin(&h, "https://relay.example"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(!valid_origin(&h, "https://relay.example"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://relay.example"),
        );
        assert!(valid_origin(&h, "https://relay.example"));
        h.append(
            header::ORIGIN,
            HeaderValue::from_static("https://relay.example"),
        );
        assert!(!valid_origin(&h, "https://relay.example"));
    }
    #[tokio::test]
    async fn main_nested_fallback_serves_relative_assets_but_not_root_routes() {
        let app = Router::new().nest(
            "/main/",
            router(RelayConfig {
                base_path: "/main/".into(),
                revision: Some("a".repeat(40)),
                unix_socket: Some("/private-test.sock".into()),
                ..config()
            })
            .unwrap(),
        );
        let response = app
            .clone()
            .oneshot(Request::get("/main/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(html.contains("./assets/"));
        let asset = html
            .split("src=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/main/{}", asset.trim_start_matches("./")))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .oneshot(Request::get("/api/mode").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    #[tokio::test]
    async fn public_routes() {
        let router = router(config()).unwrap();
        for path in ["/", "/api/mode", "/healthz", "/run", "/client/launcher.py"] {
            let response = router
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
    }
    #[test]
    fn malformed_registration_and_nonce() {
        let s = state();
        let _rx = peer(&s, 1);
        let mut role = Role::Fresh;
        for received in ["old-nonce", "nonce"] {
            assert!(
                handle(
                    &s,
                    1,
                    false,
                    "nonce",
                    &mut role,
                    Incoming::Register {
                        machine_id: "a".repeat(43),
                        public_key: "invalid".into(),
                        version: "1.0.0".into(),
                        nonce: received.into(),
                        signature: "invalid".into()
                    }
                )
                .is_err()
            );
        }
        assert!(s.registry.lock().unwrap().rooms.is_empty());
    }
    #[test]
    fn signed_registration_replay_and_duplicate() {
        use p256::ecdsa::{Signature, SigningKey, signature::Signer};
        let s = state();
        let _rx1 = peer(&s, 1);
        let _rx2 = peer(&s, 2);
        let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let public_key =
            URL_SAFE_NO_PAD.encode(key.verifying_key().to_encoded_point(false).as_bytes());
        let machine = crate::relay_crypto::machine_id(&public_key).unwrap();
        let make = |nonce: &str| {
            let transcript =
                format!("herdr-register-v1\nhttps://relay.example\n{machine}\n{nonce}\n1.0.0");
            let signature: Signature = key.sign(transcript.as_bytes());
            Incoming::Register {
                machine_id: machine.clone(),
                public_key: public_key.clone(),
                version: "1.0.0".into(),
                nonce: nonce.into(),
                signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            }
        };
        let mut owner = Role::Fresh;
        handle(&s, 1, false, "nonce1", &mut owner, make("nonce1")).unwrap();
        assert!(handle(&s, 2, false, "nonce2", &mut Role::Fresh, make("nonce1")).is_err());
        assert!(handle(&s, 2, false, "nonce2", &mut Role::Fresh, make("nonce2")).is_err());
        assert_eq!(s.registry.lock().unwrap().rooms[&machine].owner, 1);
        // Delayed cleanup of a rejected connection cannot erase the original.
        cleanup(&s, 2);
        assert_eq!(s.registry.lock().unwrap().rooms[&machine].owner, 1);
        cleanup(&s, 1);
        let _rx2 = peer(&s, 2);
        handle(&s, 2, false, "nonce2", &mut Role::Fresh, make("nonce2")).unwrap();
        cleanup(&s, 1);
        assert_eq!(s.registry.lock().unwrap().rooms[&machine].owner, 2);
    }
    #[test]
    fn routing_quotas_and_authorization() {
        let s = state();
        let mut connector_rx = peer(&s, 1);
        let mut browser_rx = peer(&s, 2);
        let machine = "a".repeat(43);
        s.registry
            .lock()
            .unwrap()
            .rooms
            .insert(machine.clone(), Room { owner: 1 });
        let mut browser = Role::Fresh;
        assert!(
            handle(
                &s,
                2,
                false,
                "n",
                &mut browser,
                Incoming::Join {
                    machine_id: machine.clone()
                }
            )
            .is_err()
        );
        handle(
            &s,
            2,
            true,
            "n",
            &mut browser,
            Incoming::Join {
                machine_id: machine.clone(),
            },
        )
        .unwrap();
        let sid = match &browser {
            Role::Browser(sid) => sid.clone(),
            _ => panic!(),
        };
        connector_rx.try_recv().unwrap();
        browser_rx.try_recv().unwrap();
        handle(
            &s,
            2,
            true,
            "n",
            &mut browser,
            Incoming::Signal {
                session_id: sid.clone(),
                payload: "opaque".into(),
            },
        )
        .unwrap();
        assert!(connector_rx.try_recv().is_ok());
        assert!(
            handle(
                &s,
                2,
                true,
                "n",
                &mut browser,
                Incoming::Authorize {
                    session_id: sid.clone()
                }
            )
            .is_err()
        );
        assert!(browser_rx.try_recv().is_err());
        let mut owner = Role::Connector(machine.clone());
        handle(
            &s,
            1,
            false,
            "n",
            &mut owner,
            Incoming::Authorize {
                session_id: sid.clone(),
            },
        )
        .unwrap();
        assert!(browser_rx.try_recv().is_ok());
        assert!(
            handle(
                &s,
                1,
                false,
                "n",
                &mut owner,
                Incoming::Authorize {
                    session_id: sid.clone()
                }
            )
            .is_err()
        );
        for id in 3..7 {
            let _rx = peer(&s, id);
            handle(
                &s,
                id,
                true,
                "n",
                &mut Role::Fresh,
                Incoming::Join {
                    machine_id: machine.clone(),
                },
            )
            .unwrap();
        }
        let _rx = peer(&s, 7);
        assert!(
            handle(
                &s,
                7,
                true,
                "n",
                &mut Role::Fresh,
                Incoming::Join {
                    machine_id: machine.clone()
                }
            )
            .is_err()
        );
        assert!(
            handle(
                &s,
                7,
                true,
                "n",
                &mut Role::Fresh,
                Incoming::Signal {
                    session_id: sid,
                    payload: "forged".into()
                }
            )
            .is_err()
        );
        cleanup(&s, 1);
        assert!(s.registry.lock().unwrap().sessions.is_empty());
        assert!(s.registry.lock().unwrap().rooms.is_empty());
    }
    #[test]
    fn authorized_sessions_do_not_expire_while_endpoints_are_connected() {
        let s = state();
        let _browser = peer(&s, 1);
        let _connector = peer(&s, 2);
        s.registry.lock().unwrap().sessions.insert(
            "active".into(),
            Session {
                machine: "machine".into(),
                browser: 1,
                connector: 2,
                authorized: true,
                expires: Instant::now() - Duration::from_secs(1),
            },
        );
        expire(&s);
        assert_eq!(s.registry.lock().unwrap().sessions.len(), 1);
        cleanup(&s, 1);
        assert!(s.registry.lock().unwrap().sessions.is_empty());
    }
    #[test]
    fn turn_rest_and_expiration() {
        let config = RelayConfig {
            turn_urls: vec!["turn:turn.example:3478".into()],
            turn_secret: Some("s".repeat(32)),
            ..config()
        };
        let servers = ice(&config, "session").unwrap();
        let username = servers[0]["username"].as_str().unwrap();
        let expiry: u64 = username.split(':').next().unwrap().parse().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!((now + 599..=now + 601).contains(&expiry));
        let mut mac =
            Hmac::<Sha1>::new_from_slice(config.turn_secret.as_ref().unwrap().as_bytes()).unwrap();
        mac.update(username.as_bytes());
        mac.verify_slice(
            &STANDARD
                .decode(servers[0]["credential"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        let s = state();
        let _rx = peer(&s, 1);
        s.registry.lock().unwrap().sessions.insert(
            "old".into(),
            Session {
                machine: "x".into(),
                browser: 1,
                connector: 2,
                authorized: false,
                expires: Instant::now() - Duration::from_secs(1),
            },
        );
        expire(&s);
        assert!(s.registry.lock().unwrap().sessions.is_empty());
    }
}
