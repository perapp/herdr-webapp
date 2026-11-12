mod auth;
mod config;
mod core;
mod dashboard;
mod terminal;
mod topology;

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
}
pub type Shared = Arc<App>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
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
    let socket = config.socket()?;
    let core = Core::new(socket.clone());
    let app = Arc::new(App {
        core: core.clone(),
        auth: Auth::new(token.clone(), config.public_origin.clone()),
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
        .unwrap_or_else(|| format!("http://{}", std::net::SocketAddr::new(host, address.port())));
    let dashboard = Arc::new(dashboard::Dashboard::start(
        &format!("{origin}/#token={token}"),
        &socket.display().to_string(),
        config.no_dashboard,
    )?);
    let mut status = core.state.subscribe();
    let logger = dashboard.clone();
    tokio::spawn(async move {
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
    tokio::spawn(core.monitor());
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await?;
    Ok(())
}

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/session", get(session))
        .route("/api/state", get(state))
        .route("/api/events", get(events))
        .route("/api/rpc", post(rpc))
        .route("/api/topology", post(topology::handle))
        .route("/api/terminal", get(terminal::upgrade))
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
        [(header::SET_COOKIE, app.auth.logout(&headers))],
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
    let path = if path.is_empty() { "index.html" } else { path };
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
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    fn app() -> Shared {
        Arc::new(App {
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
