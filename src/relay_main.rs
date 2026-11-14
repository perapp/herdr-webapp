//! Opt-in trusted-source main manager and fixed-target, streaming private proxy.
//! No source, route target or executable path comes from a public request.
use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{any, get},
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

#[derive(Clone)]
pub struct MainConfig {
    pub state_dir: PathBuf,
    pub repository: String,
    pub branch: String,
    pub interval_seconds: u64,
    pub build_jobs: u16,
    pub public_origin: String,
    pub turn_urls: Vec<String>,
    pub turn_secret_file: Option<PathBuf>,
    pub turn_ttl_seconds: u64,
    pub allow_http_loopback: bool,
}
pub struct MainService {
    config: MainConfig,
    pid: AtomicU32,
    stop: tokio::sync::watch::Sender<bool>,
    catalogs: std::sync::Mutex<Vec<(String, Arc<crate::main_catalog::MainCatalog>)>>,
}
impl MainService {
    pub fn start(config: MainConfig) -> Result<Arc<Self>> {
        ensure!(
            config.state_dir.is_absolute(),
            "main state directory must be absolute"
        );
        // Leave room for a short generation name under the portable UDS limit.
        ensure!(
            config.state_dir.as_os_str().len() <= 68,
            "main state path too long for private sockets; use --main-state-dir"
        );
        crate::relay_crypto::ensure_private_dir(&config.state_dir)?;
        ensure!(
            (1..=86400).contains(&config.interval_seconds) && (1..=16).contains(&config.build_jobs),
            "invalid main resource controls"
        );
        ensure!(
            !config.repository.starts_with('-')
                && !config.branch.starts_with('-')
                && !config.repository.chars().any(char::is_control),
            "invalid main source configuration"
        );
        let service = Arc::new(Self {
            config,
            pid: AtomicU32::new(0),
            stop: tokio::sync::watch::channel(false).0,
            catalogs: std::sync::Mutex::new(Vec::new()),
        });
        let owned = service.clone();
        tokio::spawn(async move {
            owned.supervise().await;
        });
        Ok(service)
    }
    async fn supervise(self: Arc<Self>) {
        let mut stopped = self.stop.subscribe();
        loop {
            if *stopped.borrow() {
                break;
            }
            let mut command = tokio::process::Command::new("python3");
            command
                .arg("-c")
                .arg(include_str!("../distribution/main-channel.py"))
                .arg("--state-dir")
                .arg(&self.config.state_dir)
                .arg("--repository")
                .arg(&self.config.repository)
                .arg("--branch")
                .arg(&self.config.branch)
                .arg("--public-origin")
                .arg(&self.config.public_origin)
                .arg("--interval-seconds")
                .arg(self.config.interval_seconds.to_string())
                .arg("--build-jobs")
                .arg(self.config.build_jobs.to_string())
                .arg("--turn-ttl-seconds")
                .arg(self.config.turn_ttl_seconds.to_string())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .env("HERDR_MAIN_ENTRY_PID", std::process::id().to_string())
                .process_group(0)
                .kill_on_drop(true);
            for url in &self.config.turn_urls {
                command.arg("--turn-url").arg(url);
            }
            if let Some(path) = &self.config.turn_secret_file {
                command.arg("--turn-secret-file").arg(path);
            }
            if self.config.allow_http_loopback {
                command.arg("--allow-http-loopback");
            }
            if let Ok(mut child) = command.spawn() {
                let pid = child.id().unwrap_or(0);
                self.pid.store(pid, Ordering::SeqCst);
                tokio::select! {
                    _ = child.wait() => {},
                    _ = stopped.changed() => {
                        if pid != 0 { unsafe { libc::kill(-(pid as i32), libc::SIGTERM); } }
                        if tokio::time::timeout(Duration::from_secs(15), child.wait()).await.is_err() {
                            if pid != 0 { unsafe { libc::kill(-(pid as i32), libc::SIGKILL); } }
                            let _ = child.wait().await;
                        }
                    }
                }
                self.pid.store(0, Ordering::SeqCst);
            }
            if *stopped.borrow() {
                break;
            }
            tokio::select! { _ = tokio::time::sleep(Duration::from_secs(15)) => {}, _ = stopped.changed() => {} }
        }
    }
    pub async fn shutdown(&self) {
        let _ = self.stop.send(true);
        for _ in 0..170 {
            if self.pid.load(Ordering::SeqCst) == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    fn active_socket(&self) -> Result<PathBuf> {
        let data =
            crate::relay_crypto::read_private(&self.config.state_dir.join("active.json"), 65536)?;
        let value: Value = serde_json::from_slice(&data)?;
        let revision = value["revision"]
            .as_str()
            .context("main revision missing")?;
        ensure!(valid_revision(revision), "invalid main revision");
        let leaf = value["socket"].as_str().context("main socket missing")?;
        ensure!(
            leaf.len() <= 40
                && !leaf.is_empty()
                && leaf
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                && leaf != "."
                && leaf != "..",
            "invalid private socket name"
        );
        let path = self.config.state_dir.join(leaf);
        let meta = std::fs::symlink_metadata(&path)?;
        ensure!(
            meta.file_type().is_socket()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.mode() & 0o077 == 0,
            "unsafe private main socket"
        );
        Ok(path)
    }
    fn catalog(&self, revision: &str) -> Result<Arc<crate::main_catalog::MainCatalog>> {
        ensure!(valid_revision(revision), "invalid requested revision");
        let mut cache = self.catalogs.lock().unwrap();
        if let Some(index) = cache.iter().position(|(sha, _)| sha == revision) {
            let item = cache.remove(index);
            let result = item.1.clone();
            cache.push(item);
            return Ok(result);
        }
        let catalog = Arc::new(crate::main_catalog::MainCatalog::load(
            &self
                .config
                .state_dir
                .join("artifacts")
                .join(revision)
                .join("catalog"),
            revision,
        )?);
        if cache.len() >= 3 {
            cache.remove(0);
        }
        cache.push((revision.to_owned(), catalog.clone()));
        Ok(catalog)
    }
    fn status(&self) -> Value {
        let value =
            crate::relay_crypto::read_private(&self.config.state_dir.join("status.json"), 16384)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        // Strict allowlist: never expose raw errors, paths, repositories or logs.
        let mut result = json!({"enabled":true,"phase":"starting","channel":"main"});
        if let Some(value) = value {
            if let Some(phase) = value["phase"].as_str().filter(|s| {
                [
                    "starting",
                    "fetching",
                    "building",
                    "verifying",
                    "ready",
                    "error",
                    "offline",
                    "unavailable",
                ]
                .contains(s)
            }) {
                result["phase"] = phase.into();
            }
            if let Some(revision) = value["revision"].as_str().filter(|s| valid_revision(s)) {
                result["revision"] = revision.into();
            }
        }
        result["available"] = self.active_socket().is_ok().into();
        result
    }
}
pub fn valid_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64)
        && revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
#[derive(Clone)]
struct ProxyState(Option<Arc<MainService>>);
pub fn router(service: Option<Arc<MainService>>) -> Router {
    Router::new()
        .route("/main", get(|| async { Redirect::permanent("/main/") }))
        .route("/main/_status", get(status))
        .route(
            "/main/client/{revision}/{target}/herdr-webapp",
            get(retained_binary),
        )
        .route("/main/", any(proxy))
        .route("/main/{*path}", any(proxy))
        .with_state(ProxyState(service))
}
async fn retained_binary(
    State(state): State<ProxyState>,
    axum::extract::Path((revision, target)): axum::extract::Path<(String, String)>,
) -> Response {
    let Some(service) = state.0 else {
        return unavailable(false);
    };
    if !valid_revision(&revision)
        || ![
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "aarch64-apple-darwin",
        ]
        .contains(&target.as_str())
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let sha = revision.clone();
    let catalog = tokio::task::spawn_blocking(move || service.catalog(&sha)).await;
    let Ok(Ok(catalog)) = catalog else {
        return StatusCode::NOT_FOUND.into_response();
    };
    use tower::ServiceExt;
    crate::main_catalog::router(catalog)
        .oneshot(
            Request::get(format!("/client/{revision}/{target}/herdr-webapp"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn status(State(state): State<ProxyState>) -> Response {
    let value = state.0.map_or(
        json!({"enabled":false,"available":false,"channel":"main","phase":"disabled"}),
        |s| s.status(),
    );
    ([(header::CACHE_CONTROL, "no-store")], axum::Json(value)).into_response()
}
fn unavailable(enabled: bool) -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, [(header::CACHE_CONTROL,"no-store"),(header::CONTENT_TYPE,"text/plain; charset=utf-8"),(header::X_CONTENT_TYPE_OPTIONS,"nosniff")],
      if enabled { "Main channel is building or temporarily unavailable. Stable remains available at /. Check /main/_status and retry shortly.\n" } else { "Main channel is disabled. The operator can opt in with serve-relay --enable-main.\n" }).into_response()
}
fn sanitize(headers: &mut HeaderMap, websocket: bool) {
    let nominated: Vec<_> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .map(|s| s.trim().to_owned())
        .collect();
    for name in nominated {
        if !websocket || !name.eq_ignore_ascii_case("upgrade") {
            headers.remove(name);
        }
    }
    for name in [
        "proxy-authorization",
        "proxy-authenticate",
        "keep-alive",
        "te",
        "trailer",
        "transfer-encoding",
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-herdr-peer-ip",
    ] {
        headers.remove(name);
    }
    if !websocket {
        headers.remove(header::CONNECTION);
        headers.remove(header::UPGRADE);
    }
}
async fn proxy(State(state): State<ProxyState>, mut request: Request) -> Response {
    let Some(service) = state.0 else {
        return unavailable(false);
    };
    let raw = request.uri().path();
    if raw.contains('%')
        || raw.contains('\\')
        || raw.split('/').any(|p| p == "." || p == "..")
        || request.method() == axum::http::Method::CONNECT
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if request
        .headers()
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|name| {
            [
                "origin",
                "host",
                "sec-websocket-key",
                "sec-websocket-version",
            ]
            .iter()
            .any(|protected| name.trim().eq_ignore_ascii_case(protected))
        })
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    *request.version_mut() = axum::http::Version::HTTP_11;
    let websocket = request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|h| h.as_bytes().eq_ignore_ascii_case(b"websocket"));
    if request.headers().contains_key(header::UPGRADE) && !websocket {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let downstream = websocket.then(|| hyper::upgrade::on(&mut request));
    sanitize(request.headers_mut(), websocket);
    if let Ok(origin) = service.config.public_origin.parse::<axum::http::Uri>()
        && let Some(authority) = origin.authority().and_then(|a| a.as_str().parse().ok())
    {
        request.headers_mut().insert(header::HOST, authority);
    }
    let result = async {
        let path = service.active_socket()?;
        let stream = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::UnixStream::connect(path),
        )
        .await??;
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            let _ = conn.with_upgrades().await;
        });
        let mut response =
            tokio::time::timeout(Duration::from_secs(15), sender.send_request(request)).await??;
        let upgraded = response.status() == StatusCode::SWITCHING_PROTOCOLS;
        if upgraded {
            ensure!(websocket, "unexpected upstream upgrade");
            let upstream = hyper::upgrade::on(&mut response);
            if let Some(downstream) = downstream {
                tokio::spawn(async move {
                    if let Ok((client, server)) = tokio::try_join!(downstream, upstream) {
                        let _ = tokio::io::copy_bidirectional(
                            &mut TokioIo::new(client),
                            &mut TokioIo::new(server),
                        )
                        .await;
                    }
                });
            }
        }
        sanitize(response.headers_mut(), upgraded);
        Ok::<_, anyhow::Error>(response.map(Body::new))
    }
    .await;
    result.unwrap_or_else(|_| unavailable(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    #[tokio::test]
    async fn disabled_reserves_all_main_paths_without_starting_tools() {
        for path in [
            "/main/",
            "/main/api/mode",
            "/main/run",
            "/main/client/manifest.json",
        ] {
            let response = router(None)
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        let response = router(None)
            .oneshot(Request::get("/main").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.headers()[header::LOCATION], "/main/");
    }
    #[test]
    fn revisions_and_forwarded_headers_are_strict() {
        assert!(valid_revision(&"a".repeat(40)));
        assert!(!valid_revision(&"G".repeat(40)));
        let mut headers = HeaderMap::new();
        headers.insert("connection", "x-secret, keep-alive".parse().unwrap());
        headers.insert("x-secret", "unsafe".parse().unwrap());
        headers.insert("x-forwarded-host", "evil".parse().unwrap());
        headers.insert("origin", "https://public.example".parse().unwrap());
        sanitize(&mut headers, false);
        assert!(!headers.contains_key("x-secret") && !headers.contains_key("x-forwarded-host"));
        assert_eq!(headers["origin"], "https://public.example");
    }
}

#[cfg(test)]
mod proxy_tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
    #[tokio::test]
    async fn streams_websocket_and_preserves_exact_origin_through_private_proxy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = dir.path().join("test.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let backend = Router::new().route(
            "/main/relay/socket",
            get(
                |headers: HeaderMap, ws: axum::extract::WebSocketUpgrade| async move {
                    if headers.get_all(header::ORIGIN).iter().count() != 1
                        || headers
                            .get(header::ORIGIN)
                            .is_none_or(|h| h != "https://relay.example")
                    {
                        return StatusCode::FORBIDDEN.into_response();
                    }
                    ws.on_upgrade(|mut socket| async move {
                        let _ = socket
                            .send(axum::extract::ws::Message::Text("challenge".into()))
                            .await;
                        while let Some(Ok(message)) = socket.recv().await {
                            if matches!(message, axum::extract::ws::Message::Close(_)) {
                                break;
                            }
                            if socket.send(message).await.is_err() {
                                break;
                            }
                        }
                    })
                },
            ),
        );
        let backend_task = tokio::spawn(async move {
            axum::serve(listener, backend).await.unwrap();
        });
        crate::relay_crypto::write_private(
            &dir.path().join("active.json"),
            &serde_json::to_vec(&json!({"revision":"a".repeat(40),"socket":"test.sock"})).unwrap(),
        )
        .unwrap();
        let service = Arc::new(MainService {
            config: MainConfig {
                state_dir: dir.path().to_owned(),
                repository: String::new(),
                branch: "main".into(),
                interval_seconds: 300,
                build_jobs: 1,
                public_origin: "https://relay.example".into(),
                turn_urls: vec![],
                turn_secret_file: None,
                turn_ttl_seconds: 600,
                allow_http_loopback: false,
            },
            pid: AtomicU32::new(0),
            stop: tokio::sync::watch::channel(false).0,
            catalogs: std::sync::Mutex::new(vec![]),
        });
        let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = public.local_addr().unwrap();
        let app = router(Some(service));
        let public_task = tokio::spawn(async move {
            axum::serve(public, app).await.unwrap();
        });
        for origin in ["https://evil.example", "https://relay.example"] {
            let mut request = format!("ws://{addr}/main/relay/socket")
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert(header::ORIGIN, origin.parse().unwrap());
            let result = tokio_tungstenite::connect_async(request).await;
            if origin.contains("evil") {
                assert!(
                    matches!(result, Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == StatusCode::FORBIDDEN)
                );
            } else {
                let (mut socket, _) = result.unwrap();
                assert_eq!(
                    socket.next().await.unwrap().unwrap().into_text().unwrap(),
                    "challenge"
                );
                let bytes = vec![42; 256 * 1024];
                socket
                    .send(Message::Binary(bytes.clone().into()))
                    .await
                    .unwrap();
                assert_eq!(socket.next().await.unwrap().unwrap().into_data(), bytes);
                socket.close(None).await.unwrap();
            }
        }
        let mut request = format!("ws://{addr}/main/relay/socket")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .append(header::ORIGIN, "https://relay.example".parse().unwrap());
        request
            .headers_mut()
            .append(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(
            matches!(tokio_tungstenite::connect_async(request).await, Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status()==StatusCode::FORBIDDEN)
        );
        public_task.abort();
        backend_task.abort();
    }
}
