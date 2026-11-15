mod attachments;
mod auth;
mod build_info;
mod commands;
mod component_updates;
mod config;
mod core;
mod dashboard;
mod dashboard_graphics;
mod main_catalog;
mod notifications;
mod peer_service;
mod relay_catalog;
mod relay_channel;
mod relay_connector;
mod relay_crypto;
mod relay_main;
mod relay_pairings;
mod relay_server;
mod relay_wire;
mod terminal;
mod topology;
mod update;

use auth::{ApiError, Auth, error};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, Uri, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use clap::Parser;
use config::Config;
use core::Core;
use rust_embed::Embed;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc, time::Duration};

#[derive(Embed)]
#[folder = "frontend/dist/"]
struct Assets;

pub struct App {
    pub core: Arc<Core>,
    pub auth: Auth,
    pub updates: Arc<update::Updater>,
}
pub type Shared = Arc<App>;

pub fn build_revision() -> Option<&'static str> {
    option_env!("HERDR_BUILD_REVISION").filter(|s| relay_main::valid_revision(s))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    if config.package_version {
        println!("herdr-webapp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if config.build_version {
        println!("herdr-webapp {}", build_info::VERSION);
        return Ok(());
    }
    if config.build_revision {
        println!("{}", build_revision().unwrap_or("unversioned"));
        return Ok(());
    }
    if config.licenses {
        print!(
            "{}\nLocally modified dependency: webrtc-ice 0.17.3\n{}",
            include_str!("../LICENSE"),
            include_str!("../vendor/webrtc-ice/LICENSE-MIT")
        );
        return Ok(());
    }
    match &config.mode {
        Some(config::Mode::ServeRelay(options)) => {
            let origin = config.public_origin.as_deref().ok_or_else(|| {
                anyhow::anyhow!("serve-relay requires --public-origin https://host")
            })?;
            let origin = relay_crypto::origin(origin, options.allow_http_loopback)?;
            let is_main = options.channel_base_path == "/main/";
            anyhow::ensure!(
                !options.enable_main || !is_main,
                "main child cannot recursively enable main"
            );
            if is_main {
                anyhow::ensure!(
                    options.channel_revision.as_deref() == build_revision()
                        && build_revision().is_some(),
                    "main child revision must match compiled source provenance"
                );
                if let Ok(parent) = std::env::var("HERDR_MAIN_PARENT_PID") {
                    let expected: u32 = parent.parse()?;
                    anyhow::ensure!(
                        expected > 1 && unsafe { libc::getppid() } as u32 == expected,
                        "main parent identity changed"
                    );
                    tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            if unsafe { libc::getppid() } as u32 != expected {
                                std::process::exit(0);
                            }
                        }
                    });
                }
            }
            let catalog = options
                .release_dir
                .as_ref()
                .map(|path| -> anyhow::Result<Router> {
                    if is_main {
                        Ok(main_catalog::router(Arc::new(
                            main_catalog::MainCatalog::load(
                                path,
                                options.channel_revision.as_deref().unwrap_or_default(),
                            )?,
                        )))
                    } else {
                        Ok(relay_catalog::router(Arc::new(
                            relay_catalog::ReleaseCatalog::load(path, env!("CARGO_PKG_VERSION"))?,
                        )))
                    }
                })
                .transpose()?;
            let turn_secret = options
                .turn_secret_file
                .as_ref()
                .map(|path| -> anyhow::Result<String> {
                    let bytes = relay_crypto::read_private(path, 4096)?;
                    Ok(String::from_utf8(bytes)?.trim().to_owned())
                })
                .transpose()?;
            let main = if options.enable_main {
                let state_dir = options.main_state_dir.clone().unwrap_or_else(|| {
                    let base = std::env::var_os("XDG_STATE_HOME")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| {
                            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                                .join(".local/state")
                        });
                    base.join("herdr-webapp/main")
                });
                Some(relay_main::MainConfig {
                    state_dir: std::path::absolute(state_dir)?,
                    source: options.main_source,
                    artifact_base_url: options.main_artifact_base_url.clone(),
                    repository: options.main_repository.clone(),
                    branch: options.main_branch.clone(),
                    interval_seconds: options.main_update_interval,
                    build_jobs: options.main_build_jobs,
                    public_origin: origin.clone(),
                    turn_urls: options.turn_urls.clone(),
                    turn_secret_file: options
                        .turn_secret_file
                        .as_ref()
                        .map(std::path::absolute)
                        .transpose()?,
                    turn_ttl_seconds: options.turn_ttl_seconds,
                    allow_http_loopback: options.allow_http_loopback,
                })
            } else {
                None
            };
            return relay_server::run(
                relay_server::RelayConfig {
                    public_origin: origin,
                    base_path: options.channel_base_path.clone(),
                    revision: options.channel_revision.clone(),
                    unix_socket: options
                        .unix_socket
                        .as_ref()
                        .map(std::path::absolute)
                        .transpose()?,
                    main,
                    bind: config.bind,
                    port: config.port.unwrap_or(8080),
                    allow_insecure_loopback: options.allow_http_loopback,
                    turn_urls: options.turn_urls.clone(),
                    turn_secret,
                    turn_ttl_seconds: options.turn_ttl_seconds,
                    ..Default::default()
                },
                catalog,
            )
            .await;
        }
        Some(config::Mode::ConnectRelay(options)) => {
            use sha2::{Digest, Sha256};
            let endpoint = relay_channel::RelayEndpoint::parse(
                &options.relay_url,
                options.allow_http_loopback,
            )?;
            let origin = endpoint.url();
            let state_dir = match &options.state_dir {
                Some(path) => std::path::absolute(path)?,
                None => {
                    let base = std::env::var_os("XDG_STATE_HOME")
                        .map(std::path::PathBuf::from)
                        .unwrap_or(
                            std::path::PathBuf::from(
                                std::env::var_os("HOME")
                                    .ok_or_else(|| anyhow::anyhow!("HOME is required"))?,
                            )
                            .join(".local/state"),
                        );
                    base.join("herdr-webapp/relay").join(
                        Sha256::digest(origin.as_bytes())
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>(),
                    )
                }
            };
            return relay_connector::run(relay_connector::ConnectorConfig {
                relay_url: origin,
                core_socket: std::path::absolute(config.socket()?)?,
                state_dir,
                ready_file: options
                    .ready_file
                    .as_ref()
                    .map(std::path::absolute)
                    .transpose()?,
                allow_http_loopback: options.allow_http_loopback,
                force_turn: options.force_turn,
                reset_pairings: options.reset_pairings,
                no_dashboard: config.no_dashboard,
            })
            .await;
        }
        Some(config::Mode::Local) | None => {}
    }
    if let Some(plan) = &config.update_helper {
        // Deliberately do not expose plan paths or subprocess diagnostics.
        return update::helper(plan).map_err(|_| {
            anyhow::anyhow!(
                "Update helper failed; inspect managed installation backup and lock before recovery"
            )
        });
    }
    if let Some(origin) = &config.public_origin {
        let uri: Uri = origin.parse()?;
        anyhow::ensure!(
            matches!(uri.scheme_str(), Some("http" | "https"))
                && uri.authority().is_some()
                && uri.path() == "/"
                && !origin.ends_with('/')
                && uri.query().is_none(),
            "--public-origin must be exactly scheme://host[:port], without a trailing slash or path"
        );
    }
    let token = config.token()?;
    let socket = std::path::absolute(config.socket()?)?;
    let token_path = std::path::absolute(config.token_path()?)?;
    let updates = update::Updater::startup()?;
    let core = Core::new(socket.clone());
    let notification_dir = token_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Token parent missing"))?
        .join("web-push");
    let notifications = notifications::Notifications::open(&notification_dir)?;
    let _ = core.notifications.set(notifications.clone());
    tokio::spawn(notifications.observe(core.notification_events.subscribe()));
    let app = Arc::new(App {
        core: core.clone(),
        updates: updates.clone(),
        auth: Auth::persistent(
            token.clone(),
            config.public_origin.clone(),
            config.session_path()?,
        )?,
    });
    let listener = if let Some(port) = config.port {
        tokio::net::TcpListener::bind((config.bind, port)).await?
    } else {
        let mut found = None;
        for port in 8001..8101 {
            match tokio::net::TcpListener::bind((config.bind, port)).await {
                Ok(l) => {
                    found = Some(l);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
                Err(e) => return Err(e.into()),
            }
        }
        found.ok_or_else(|| {
            anyhow::anyhow!("no free stable-app port from 8001 through 8100; use --port")
        })?
    };
    let address = listener.local_addr()?;
    updates.run_ready();
    let host = if config.bind.is_unspecified() {
        let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
        sock.connect("192.0.2.1:80").ok();
        sock.local_addr()
            .ok()
            .filter(|a| !a.ip().is_unspecified())
            .map(|a| a.ip())
            .unwrap_or(std::net::IpAddr::from([127, 0, 0, 1]))
    } else {
        config.bind
    };
    let origin = config
        .public_origin
        .clone()
        .unwrap_or_else(|| format!("http://{}", std::net::SocketAddr::new(host, address.port())));
    let dashboard = Arc::new(dashboard::Dashboard::start(
        &format!("{origin}/#token={token}"),
        &socket.display().to_string(),
        config.no_dashboard,
        dashboard::Mode::Local,
    )?);
    let mut status = core.state.subscribe();
    let logger = dashboard.clone();
    let status_logger = tokio::spawn(async move {
        let mut previous = String::new();
        while status.changed().await.is_ok() {
            let state = status.borrow_and_update();
            let message = if state["connected"] == true {
                "Core connected; topology and terminal status are live".to_owned()
            } else {
                format!(
                    "Core disconnected: {}",
                    state["error"].as_str().unwrap_or("unknown error")
                )
            };
            if previous != message {
                logger.log(&message);
                previous = message;
            }
        }
    });
    let mut restart_args = vec![
        "--bind".into(),
        config.bind.to_string(),
        "--port".into(),
        address.port().to_string(),
        "--core-socket".into(),
        socket.to_string_lossy().into_owned(),
        "--token-file".into(),
        token_path.to_string_lossy().into_owned(),
    ];
    if let Some(origin) = config.public_origin {
        restart_args.extend(["--public-origin".into(), origin]);
    }
    if config.no_dashboard {
        restart_args.push("--no-dashboard".into());
    }
    updates.configure(restart_args);
    updates.clone().automatic();
    tokio::spawn(core.monitor());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serve = axum::serve(listener, router(app)).with_graceful_shutdown(async {
        let _ = stopped.await;
    });
    let mut serve = Box::pin(std::future::IntoFuture::into_future(serve));
    tokio::select! {
        result = &mut serve => result?,
        _ = tokio::signal::ctrl_c() => { let _ = stop.send(()); let _ = tokio::time::timeout(Duration::from_secs(2), &mut serve).await; },
        _ = updates.shutdown.notified() => {
            // Long-lived SSE/WS clients must not hold the web process open indefinitely.
            let _ = stop.send(());
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut serve).await;
        },
    }
    // Stop the only dashboard-owning task and restore its screen before exec.
    status_logger.abort();
    let _ = status_logger.await;
    drop(dashboard);
    // In-place exec retains the foreground shell job and original service/TTY
    // stdio. CLOEXEC closes auth locks and sockets even if Arc tasks still exist.
    updates.exec_pending()?;
    Ok(())
}

fn local_mode() -> Value {
    json!({"mode":"local","version":relay_crypto::VERSION,"protocolVersion":relay_crypto::PROTOCOL_VERSION,"build":build_info::BuildInfo::current()})
}

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/api/mode", get(|| async { Json(local_mode()) }))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/session", get(session))
        .route("/api/state", get(state))
        .route("/api/commands", get(commands::handle))
        .route("/api/events", get(events))
        .route("/api/rpc", post(rpc))
        .route("/api/topology", post(topology::handle))
        .route("/api/terminal", get(terminal::upgrade))
        .merge(notifications::routes())
        .merge(attachments::routes())
        .merge(update::routes())
        .merge(component_updates::routes())
        .fallback(get(asset))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(axum::middleware::from_fn(auth::security_headers))
        .with_state(app)
}
#[derive(Deserialize)]
struct Login {
    token: String,
}
async fn login(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Login>,
) -> Result<Response, ApiError> {
    app.auth.same_origin(&headers, false)?;
    match app.auth.login(&body.token) {
        Ok(cookie) => Ok((
            [(header::SET_COOKIE, cookie)],
            Json(json!({"authenticated":true})),
        )
            .into_response()),
        Err(e) => {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Err(e)
        }
    }
}
async fn logout(State(app): State<Shared>, headers: HeaderMap) -> Result<Response, ApiError> {
    app.auth.require(&headers)?;
    app.auth.same_origin(&headers, false)?;
    Ok((
        [(header::SET_COOKIE, app.auth.logout(&headers)?)],
        Json(json!({"authenticated":false})),
    )
        .into_response())
}
async fn session(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    app.auth.require(&headers)?;
    Ok(Json(json!({"authenticated":true})))
}
async fn state(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    app.auth.require(&headers)?;
    Ok(Json(app.core.state.borrow().clone()))
}
async fn events(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    app.auth.require(&headers)?;
    let mut state = app.core.state.subscribe();
    Ok(Sse::new(async_stream::stream! {
        loop {
            if !app.auth.authorized(&headers){break;}
            let data=state.borrow_and_update().to_string();
            yield Ok(Event::default().event("state").data(data));
            tokio::select! {
                result=state.changed()=>if result.is_err(){break;},
                _=tokio::time::sleep(Duration::from_secs(15))=>{},
            }
        }
    })
    .keep_alive(KeepAlive::default()))
}
#[derive(Deserialize)]
struct Rpc {
    method: String,
    #[serde(default = "empty_object")]
    params: Value,
}
fn empty_object() -> Value {
    json!({})
}
async fn rpc(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Rpc>,
) -> Result<Json<Value>, ApiError> {
    app.auth.require(&headers)?;
    app.auth.same_origin(&headers, false)?;
    // This is API surface control, not a shell sandbox. Authenticated users are fully trusted.
    let allowed = [
        "terminal.get",
        "terminal.run",
        "terminal.send",
        "terminal.keys",
        "terminal.interrupt",
        "terminal.list",
        "terminal.label",
        "doc.get",
        "machine.list",
    ];
    if !allowed.contains(&body.method.as_str()) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Method is not exposed by this initial web client",
        ));
    }
    let result = app.core.call(&body.method, body.params).await;
    Ok(Json(match result {
        Ok(v) => json!({"result":v}),
        Err(e) => json!({"error":{"message":e.to_string()}}),
    }))
}
async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Local installations resume the normal cookie-authenticated UI. Other
    // unknown paths remain 404; remembered computer routes belong to the relay.
    let path = if path.is_empty() || path == "launch" {
        "index.html"
    } else {
        path
    };
    if let Some(file) = Assets::get(path) {
        (
            [(
                header::CONTENT_TYPE,
                mime_guess::from_path(path)
                    .first_or_octet_stream()
                    .to_string(),
            )],
            file.data,
        )
            .into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_mode_adds_build_without_changing_legacy_fields() {
        let mode = local_mode();
        assert_eq!(mode["mode"], "local");
        assert_eq!(mode["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(mode["build"]["version"], build_info::VERSION);
        assert_eq!(mode["protocolVersion"], relay_crypto::PROTOCOL_VERSION);
        assert_eq!(
            mode["build"],
            serde_json::to_value(build_info::BuildInfo::current()).unwrap()
        );
    }

    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    fn app() -> Shared {
        Arc::new(App {
            updates: update::Updater::development(),
            core: Core::new("/nonexistent".into()),
            auth: Auth::new("a".repeat(64), None),
        })
    }
    #[tokio::test]
    async fn protected_api_and_assets() {
        let a = router(app());
        let response = a
            .clone()
            .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        let response = a
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(response.headers().contains_key("content-security-policy"));
        let response = a
            .clone()
            .oneshot(Request::get("/api/nope").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
        let response = a
            .oneshot(
                Request::get("/api/state")
                    .header("authorization", format!("Bearer {}", "a".repeat(64)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
    #[tokio::test]
    async fn foreign_origin_login_denied() {
        let response = router(app())
            .oneshot(
                Request::post("/api/login")
                    .header("content-type", "application/json")
                    .header("host", "localhost:8001")
                    .header("origin", "http://evil.example")
                    .body(Body::from(json!({"token":"a".repeat(64)}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 403);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        use http_body_util::BodyExt;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["error"]["message"],
            "Origin does not match this server"
        );
    }
    #[tokio::test]
    async fn lan_browser_login_and_invalid_token_diagnostics() {
        let server = router(app());
        let request = |token: &str| {
            Request::post("/api/login")
                .header("content-type", "application/json")
                .header("host", "192.168.1.234:8001")
                .header("origin", "http://192.168.1.234:8001")
                .body(Body::from(json!({"token":token}).to_string()))
                .unwrap()
        };
        let ok = server
            .clone()
            .oneshot(request(&"a".repeat(64)))
            .await
            .unwrap();
        assert_eq!(ok.status(), 200);
        assert!(
            ok.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("HttpOnly")
        );
        let bad = server.oneshot(request("incorrect")).await.unwrap();
        assert_eq!(bad.status(), 401);
        use http_body_util::BodyExt;
        let body = bad.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["message"], "Invalid access token");
    }
}
