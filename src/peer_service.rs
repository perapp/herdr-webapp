//! Private application adapter for an already cryptographically authenticated peer.
//!
//! The owner MUST verify the peer/session and control hello before calling
//! `serve_channel`, and cancel ALL channel futures on session/PC loss. This type
//! is not a peer authenticator. Never hand its capability or headers to a browser.
//! It creates no listener and uses the existing router/native terminal bridge.
//! Transport owns SCTP fragmentation/reassembly (<=16 KiB fragments); a logical
//! binary terminal message may be the Core's full 256 KiB credit window.
use crate::{
    App, Shared,
    auth::Auth,
    core::Core,
    terminal::{self, TerminalWire},
};
use anyhow::{Result, bail, ensure};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ws::Message,
    http::{HeaderMap, Request, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    sync::{Semaphore, mpsc},
    task::{AbortHandle, JoinSet},
};
use tower::ServiceExt;

const ORIGIN: &str = "http://herdr-peer.internal";
const MAX_BODY: usize = 1 << 20;
const MAX_TEXT: usize = 1500 * 1024;
const MAX_RESPONSE: usize = 8 << 20;
const SEND_TIMEOUT: Duration = Duration::from_secs(2);
const OP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum PeerMessage {
    Text(String),
    Binary(Vec<u8>),
}

/// One private authorization context per verified peer session. Clone it to
/// serve concurrent channels of that session; do not reuse it across sessions.
#[derive(Clone)]
pub struct PeerService {
    app: Shared,
    router: Router,
    headers: HeaderMap,
    requests: Arc<Semaphore>,
    subscriptions: Arc<Semaphore>,
    terminals: Arc<Semaphore>,
    uploads: Arc<Semaphore>,
    owner: crate::notifications::Owner,
    access: Option<Arc<tokio::sync::Mutex<crate::relay_pairings::Pairings>>>,
    dashboard: Option<crate::dashboard::DashboardHandle>,
    auth_generation: Option<u64>,
}
impl PeerService {
    #[cfg(test)]
    pub fn new(core: Arc<Core>) -> Self {
        Self::for_peer(core, "local".into())
    }
    pub fn for_peer(core: Arc<Core>, owner: String) -> Self {
        let mut bytes = [0; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let app = Arc::new(App {
            core,
            auth: Auth::new(token.clone(), Some(ORIGIN.into())),
            updates: crate::update::Updater::development(),
        });
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers.insert(header::ORIGIN, ORIGIN.parse().unwrap());
        headers.insert(header::HOST, "herdr-peer.internal".parse().unwrap());
        Self {
            router: crate::router(app.clone()),
            app,
            headers,
            requests: Arc::new(Semaphore::new(32)),
            subscriptions: Arc::new(Semaphore::new(32)),
            terminals: Arc::new(Semaphore::new(32)),
            uploads: Arc::new(Semaphore::new(2)),
            owner: crate::notifications::Owner(owner),
            access: None,
            dashboard: None,
            auth_generation: None,
        }
    }

    /// Browser identity is the connector-verified signing key, never the private
    /// router bearer or a request-provided retainKey.
    pub fn with_access(
        mut self,
        pairings: Arc<tokio::sync::Mutex<crate::relay_pairings::Pairings>>,
    ) -> Self {
        self.access = Some(pairings);
        self
    }

    pub fn with_generation(mut self, generation: u64) -> Self {
        self.auth_generation = Some(generation);
        self
    }
    pub fn with_dashboard(mut self, dashboard: Option<crate::dashboard::DashboardHandle>) -> Self {
        self.dashboard = dashboard;
        self
    }

    /// Only call after authenticated session hello. Drop this future to cancel;
    /// no input, mutation or upload is retried after transport loss.
    /// Control POST bodies are standard-base64 JSON, defaulting to application/json.
    /// Upload metadata uses contentType: application/octet-stream; size is exact.
    /// Unknown routes (including local updater/login/logout) are never dispatched.
    pub async fn serve_channel(
        &self,
        label: &str,
        incoming: mpsc::Receiver<PeerMessage>,
        outgoing: mpsc::Sender<PeerMessage>,
    ) -> Result<()> {
        let Some(shared) = &self.access else {
            return self.serve_channel_inner(label, incoming, outgoing).await;
        };
        let (generation, mut changes) = {
            let pairings = shared.lock().await;
            let generation = self.auth_generation.unwrap_or(pairings.generation());
            ensure!(
                pairings.authorized_at(&self.owner.0, generation),
                "browser revoked"
            );
            (generation, pairings.subscribe())
        };
        let channel = self.serve_channel_inner(label, incoming, outgoing);
        tokio::pin!(channel);
        loop {
            tokio::select! {
                biased;
                changed = changes.changed() => {
                    changed?;
                    ensure!(shared.lock().await.authorized_at(&self.owner.0, generation), "browser revoked");
                },
                result = &mut channel => return result,
            }
        }
    }
    async fn serve_channel_inner(
        &self,
        label: &str,
        incoming: mpsc::Receiver<PeerMessage>,
        outgoing: mpsc::Sender<PeerMessage>,
    ) -> Result<()> {
        if label == "herdr-control-v1" {
            return self.control(incoming, outgoing).await;
        }
        if valid_label(label, "herdr-terminal-v1:") {
            let _permit = self.terminals.clone().try_acquire_owned()?;
            let mut wire = PeerWire { incoming, outgoing };
            let first = tokio::time::timeout(Duration::from_secs(10), wire.incoming.recv()).await?;
            let Some(PeerMessage::Text(text)) = first else {
                bail!("expected terminal attach");
            };
            ensure!(text.len() <= 65536, "attach too large");
            let query = serde_json::from_str::<terminal::AttachQuery>(&text)
                .map_err(|_| anyhow::anyhow!("invalid terminal attach"))?;
            let result = terminal::session(&mut wire, &self.app, &self.headers, query).await;
            if result.is_err() {
                let _ = wire.send(Message::Text(json!({"type":"error","message":"Terminal connection ended; reconnect to try again"}).to_string().into())).await;
            }
            return result;
        }
        if valid_label(label, "herdr-upload-v1:") {
            let _permit = self.uploads.clone().try_acquire_owned()?;
            return self.upload(incoming, outgoing).await;
        }
        bail!("unsupported peer channel")
    }

    async fn dispatch(
        &self,
        method: &str,
        path: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<axum::response::Response> {
        // Serialize authorization-sensitive dispatch with rotation. The internal
        // Auth bearer is intentionally never rotated by this operation.
        let mut access = match &self.access {
            Some(pairings) => Some(pairings.lock().await),
            None => None,
        };
        if let Some(pairings) = &access {
            ensure!(
                self.auth_generation.map_or_else(
                    || pairings.known(&self.owner.0),
                    |generation| pairings.authorized_at(&self.owner.0, generation)
                ),
                "browser revoked"
            );
        }
        if matches!(
            path,
            "/api/access" | "/api/access/rotate" | "/api/access/link"
        ) {
            let Some(pairings) = access.as_mut() else {
                bail!("access operation unavailable");
            };
            let value = match (method, path) {
                ("GET", "/api/access") => {
                    json!({"supported":true,"canRotate":true,"scope":"computer","tokenLifetime":"untilRotated","sessionLifetime":"untilRevoked"})
                }
                ("POST", "/api/access/rotate") => {
                    ensure!(
                        serde_json::from_slice::<Value>(&body)? == json!({}),
                        "expected empty object"
                    );
                    pairings.rotate(&self.owner.0)?;
                    if let Some(dashboard) = &self.dashboard {
                        dashboard.hide_private_link();
                    }
                    if let Some(notifications) = self.app.core.notifications.get() {
                        notifications.keep_owner(&self.owner.0).await?;
                    }
                    json!({"rotated":true})
                }
                // The pairing capability, never the private router bearer. It
                // travels only inside this end-to-end encrypted channel, to a
                // browser whose key is already trusted.
                ("POST", "/api/access/link") => {
                    ensure!(
                        serde_json::from_slice::<Value>(&body)? == json!({}),
                        "expected empty object"
                    );
                    ensure!(pairings.available(), "pairing limit reached");
                    let capability = serde_json::to_vec(&pairings.capability)?;
                    json!({"fragment":format!(
                        "pair={}",
                        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(capability)
                    )})
                }
                _ => bail!("invalid access operation"),
            };
            return Ok(axum::response::IntoResponse::into_response(axum::Json(
                value,
            )));
        }
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::from(body))?;
        *request.headers_mut() = self.headers.clone();
        request.extensions_mut().insert(self.owner.clone());
        if let Some(value) = content_type {
            request
                .headers_mut()
                .insert(header::CONTENT_TYPE, value.parse()?);
        }
        Ok(tokio::time::timeout(OP_TIMEOUT, self.router.clone().oneshot(request)).await??)
    }

    async fn request(
        &self,
        id: Value,
        method: String,
        path: String,
        body: Option<String>,
        content_type: Option<String>,
        out: mpsc::Sender<PeerMessage>,
    ) -> Result<()> {
        let result: Result<(u16, Vec<u8>)> = async {
            ensure!(allowed(&method, &path), "route not allowed");
            ensure!(
                method != "GET" || (body.is_none() && content_type.is_none()),
                "GET body not allowed"
            );
            ensure!(
                content_type
                    .as_deref()
                    .is_none_or(|v| v == "application/json"),
                "unsupported content type"
            );
            let bytes = match body {
                Some(b) => STANDARD.decode(b)?,
                None => Vec::new(),
            };
            ensure!(bytes.len() <= MAX_BODY, "request too large");
            let content_type = content_type
                .as_deref()
                .or_else(|| (method == "POST").then_some("application/json"));
            let response = self.dispatch(&method, &path, bytes, content_type).await?;
            let status = response.status().as_u16();
            let body =
                tokio::time::timeout(OP_TIMEOUT, to_bytes(response.into_body(), MAX_RESPONSE))
                    .await??;
            Ok((status, body.to_vec()))
        }
        .await;
        let (status, body) = result.unwrap_or_else(|_| {
            (
                400,
                b"{\"error\":{\"message\":\"Invalid or unavailable peer request\"}}".to_vec(),
            )
        });
        send(
            &out,
            json!({"type":"response","id":id,"status":status,"body":STANDARD.encode(body)}),
        )
        .await
    }

    async fn control(
        &self,
        mut incoming: mpsc::Receiver<PeerMessage>,
        outgoing: mpsc::Sender<PeerMessage>,
    ) -> Result<()> {
        // JoinSet aborts every child on cancellation, including long-lived SSE bodies.
        let mut tasks = JoinSet::new();
        let mut active: HashMap<String, (u64, AbortHandle)> = HashMap::new();
        let mut generation = 0u64;
        loop {
            tokio::select! {
                _ = outgoing.closed() => return Ok(()),
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(Ok((key, epoch, result))) = result
                        && active.get(&key).is_some_and(|(current, _)| *current == epoch) {
                            active.remove(&key);
                            result?;
                    }
                }
                message = incoming.recv() => {
                    let Some(PeerMessage::Text(text)) = message else { return Ok(()); };
                    ensure!(text.len() <= MAX_TEXT, "control message too large");
                    let command: Control = serde_json::from_str(&text).map_err(|_| anyhow::anyhow!("invalid control message"))?;
                    let id = command.id().clone();
                    ensure!(valid_id(&id), "invalid request id");
                    let key = id.to_string();
                    if matches!(command, Control::Cancel { .. }) {
                        if let Some((_, task)) = active.remove(&key) { task.abort(); }
                        send(&outgoing, json!({"type":"closed","id":id})).await?;
                        continue;
                    }
                    ensure!(!active.contains_key(&key), "duplicate active id");
                    generation = generation.checked_add(1).ok_or_else(|| anyhow::anyhow!("request generation exhausted"))?;
                    let epoch = generation;
                    let service = self.clone();
                    let out = outgoing.clone();
                    let task_key = key.clone();
                    let handle = match command {
                        Control::Request { id, method, path, body, content_type } => {
                            match self.requests.clone().try_acquire_owned() {
                                Ok(permit) => tasks.spawn(async move { let _permit = permit; let result = service.request(id, method, path, body, content_type, out).await; (task_key, epoch, result) }),
                                Err(_) => { send(&outgoing, json!({"type":"response","id":id,"status":429,"body":STANDARD.encode(b"Too many requests")})).await?; continue; }
                            }
                        }
                        Control::Subscribe { id, path } => {
                            ensure!(path == "/api/events", "subscription route not allowed");
                            match self.subscriptions.clone().try_acquire_owned() {
                                Ok(permit) => tasks.spawn(async move { let _permit = permit; let result = service.subscribe(id, out).await; (task_key, epoch, result) }),
                                Err(_) => { send(&outgoing, json!({"type":"closed","id":id})).await?; continue; }
                            }
                        }
                        Control::Cancel { .. } => unreachable!(),
                    };
                    active.insert(key, (epoch, handle));
                }
            }
        }
    }

    async fn subscribe(&self, id: Value, out: mpsc::Sender<PeerMessage>) -> Result<()> {
        let response = self
            .dispatch("GET", "/api/events", Vec::new(), None)
            .await?;
        ensure!(response.status().is_success(), "subscription refused");
        send(&out, json!({"type":"subscribed","id":id})).await?;
        let mut stream = response.into_body().into_data_stream();
        let mut buffer = Vec::new();
        while let Some(chunk) = stream.next().await {
            buffer.extend_from_slice(&chunk?);
            ensure!(buffer.len() <= MAX_RESPONSE, "event too large");
            while let Some(end) = buffer.windows(2).position(|w| w == b"\n\n") {
                let block = String::from_utf8(buffer.drain(..end + 2).collect())?;
                let mut event = "message";
                let mut data = Vec::new();
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        event = v.strip_prefix(' ').unwrap_or(v);
                    }
                    if let Some(v) = line.strip_prefix("data:") {
                        data.push(v.strip_prefix(' ').unwrap_or(v));
                    }
                }
                if !data.is_empty() {
                    send(
                        &out,
                        json!({"type":"event","id":id,"event":event,"data":data.join("\n")}),
                    )
                    .await?;
                }
            }
        }
        send(&out, json!({"type":"closed","id":id})).await
    }

    async fn upload(
        &self,
        mut incoming: mpsc::Receiver<PeerMessage>,
        outgoing: mpsc::Sender<PeerMessage>,
    ) -> Result<()> {
        let result: Result<(u16, Vec<u8>)> = tokio::time::timeout(OP_TIMEOUT, async {
            let Some(PeerMessage::Text(text)) = incoming.recv().await else {
                bail!("expected upload metadata");
            };
            ensure!(text.len() <= 4096, "metadata too large");
            let metadata: Upload = serde_json::from_str(&text)
                .map_err(|_| anyhow::anyhow!("invalid upload metadata"))?;
            ensure!(
                metadata.kind == "upload" && metadata.size > 0 && metadata.size <= 8 << 20,
                "invalid upload size"
            );
            ensure!(
                !metadata.terminal.is_empty()
                    && metadata.terminal.len() <= 512
                    && !metadata.terminal.chars().any(char::is_control),
                "invalid terminal"
            );
            ensure!(
                metadata.content_type == "application/octet-stream",
                "unsupported upload type"
            );
            let mut body = Vec::with_capacity(metadata.size);
            while body.len() < metadata.size {
                let Some(PeerMessage::Binary(chunk)) = incoming.recv().await else {
                    bail!("upload interrupted");
                };
                ensure!(
                    !chunk.is_empty()
                        && chunk.len() <= 16384
                        && chunk.len() <= metadata.size - body.len(),
                    "invalid upload chunk"
                );
                body.extend(chunk);
            }
            // Percent-encode every byte, so a terminal address cannot introduce query fields.
            let terminal: String = metadata
                .terminal
                .bytes()
                .map(|b| format!("%{b:02X}"))
                .collect();
            let response = self
                .dispatch(
                    "POST",
                    &format!("/api/attachments?terminal={terminal}"),
                    body,
                    Some("application/octet-stream"),
                )
                .await?;
            let status = response.status().as_u16();
            let body = to_bytes(response.into_body(), MAX_RESPONSE).await?;
            Ok((status, body.to_vec()))
        })
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("upload timeout")));
        let (status, body) = result.unwrap_or_else(|_| (400, b"{\"error\":{\"message\":\"Upload invalid, interrupted or timed out; no retry made\"}}".to_vec()));
        send(
            &outgoing,
            json!({"type":"upload_result","status":status,"body":STANDARD.encode(body)}),
        )
        .await
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Control {
    Request {
        id: Value,
        method: String,
        path: String,
        body: Option<String>,
        #[serde(rename = "contentType")]
        content_type: Option<String>,
    },
    Subscribe {
        id: Value,
        path: String,
    },
    Cancel {
        id: Value,
    },
}
impl Control {
    fn id(&self) -> &Value {
        match self {
            Self::Request { id, .. } | Self::Subscribe { id, .. } | Self::Cancel { id } => id,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Upload {
    #[serde(rename = "type")]
    kind: String,
    terminal: String,
    size: usize,
    #[serde(rename = "contentType")]
    content_type: String,
}
fn valid_id(id: &Value) -> bool {
    id.as_str()
        .is_some_and(|v| !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control))
        || id.as_u64().is_some_and(|v| v <= 9_007_199_254_740_991)
}
fn valid_label(label: &str, prefix: &str) -> bool {
    label.strip_prefix(prefix).is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    })
}
fn allowed(method: &str, path: &str) -> bool {
    match (method, path) {
        (
            "GET",
            "/api/session"
            | "/api/state"
            | "/api/notifications"
            | "/api/component-updates"
            | "/api/access",
        )
        | (
            "POST",
            "/api/rpc"
            | "/api/access/rotate"
            | "/api/access/link"
            | "/api/component-updates/check"
            | "/api/component-updates/install"
            | "/api/topology"
            | "/api/notifications"
            | "/api/notifications/delete"
            | "/api/notifications/seen",
        ) => true,
        ("GET", p) => p.strip_prefix("/api/commands?terminal=").is_some_and(|q| {
            !q.is_empty()
                && q.len() <= 1536
                && q.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"%-_./".contains(&b))
        }),
        _ => false,
    }
}
async fn send(out: &mpsc::Sender<PeerMessage>, value: Value) -> Result<()> {
    tokio::time::timeout(SEND_TIMEOUT, out.send(PeerMessage::Text(value.to_string())))
        .await?
        .map_err(|_| anyhow::anyhow!("peer channel closed"))
}
struct PeerWire {
    incoming: mpsc::Receiver<PeerMessage>,
    outgoing: mpsc::Sender<PeerMessage>,
}
impl TerminalWire for PeerWire {
    async fn send(&mut self, message: Message) -> Result<()> {
        let message = match message {
            Message::Text(t) => PeerMessage::Text(t.to_string()),
            Message::Binary(b) => PeerMessage::Binary(b.to_vec()),
            Message::Close(_) => return Ok(()),
            _ => return Ok(()),
        };
        tokio::time::timeout(SEND_TIMEOUT, self.outgoing.send(message))
            .await?
            .map_err(|_| anyhow::anyhow!("peer channel closed"))
    }
    async fn recv(&mut self) -> Option<Result<Message>> {
        tokio::select! {
            _ = self.outgoing.closed() => None,
            message = self.incoming.recv() => message.map(|m| Ok(match m { PeerMessage::Text(t) => Message::Text(t.into()), PeerMessage::Binary(b) => Message::Binary(b.into()) })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn access_rotation_uses_verified_owner_not_internal_bearer() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let a = crate::relay_crypto::Identity::load_or_create(&dir.path().join("a"))
            .unwrap()
            .public_key();
        let b = crate::relay_crypto::Identity::load_or_create(&dir.path().join("b"))
            .unwrap()
            .public_key();
        let mut pairings =
            crate::relay_pairings::Pairings::load(dir.path(), "m".into(), "p".into(), false)
                .unwrap();
        for key in [&a, &b] {
            let proof =
                crate::relay_crypto::pair_proof(&pairings.capability.secret, b"auth").unwrap();
            pairings.reserve("s", b"auth", &proof).unwrap();
            pairings.complete("s", key, true).unwrap();
        }
        let shared = Arc::new(tokio::sync::Mutex::new(pairings));
        let core = Core::new("/nonexistent".into());
        let current = PeerService::for_peer(core.clone(), a.clone()).with_access(shared.clone());
        let same_browser =
            PeerService::for_peer(core.clone(), a.clone()).with_access(shared.clone());
        let revoked = PeerService::for_peer(core, b.clone()).with_access(shared.clone());
        let original_headers = current.headers.clone();
        let (kept_tx, kept_rx) = mpsc::channel(4);
        let (kept_out, mut kept_responses) = mpsc::channel(4);
        let kept_service = same_browser.clone();
        let kept_task = tokio::spawn(async move {
            kept_service
                .serve_channel("herdr-control-v1", kept_rx, kept_out)
                .await
        });
        let (revoked_tx, revoked_rx) = mpsc::channel(4);
        let (revoked_out, mut revoked_responses) = mpsc::channel(4);
        let revoked_service = revoked.clone();
        let revoked_task = tokio::spawn(async move {
            revoked_service
                .serve_channel("herdr-control-v1", revoked_rx, revoked_out)
                .await
        });
        for tx in [&kept_tx, &revoked_tx] {
            tx.send(PeerMessage::Text(
                json!({"type":"request","id":1,"method":"GET","path":"/api/session"}).to_string(),
            ))
            .await
            .unwrap();
        }
        assert!(kept_responses.recv().await.is_some());
        assert!(revoked_responses.recv().await.is_some());
        assert!(
            current
                .dispatch(
                    "POST",
                    "/api/access/rotate",
                    br#"{"retainKey":"other"}"#.to_vec(),
                    None
                )
                .await
                .is_err()
        );
        let response = current
            .dispatch("GET", "/api/access", vec![], None)
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"supported":true,"canRotate":true,"scope":"computer","tokenLifetime":"untilRotated","sessionLifetime":"untilRevoked"})
        );
        // The link is the pairing capability, never the internal router bearer.
        let response = current
            .dispatch("POST", "/api/access/link", b"{}".to_vec(), None)
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        let fragment = body["fragment"].as_str().unwrap();
        let capability: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(fragment.strip_prefix("pair=").unwrap())
                .unwrap(),
        )
        .unwrap();
        let secret = shared.lock().await.capability.secret.clone();
        assert_eq!(capability["secret"], json!(secret));
        assert!(!fragment.contains("Bearer"));
        assert!(
            current
                .dispatch("POST", "/api/access/link", b"{\"x\":1}".to_vec(), None)
                .await
                .is_err()
        );
        assert!(
            current
                .dispatch("POST", "/api/access/rotate", b"{}".to_vec(), None)
                .await
                .unwrap()
                .status()
                .is_success()
        );
        assert!(current.app.auth.authorized(&original_headers));
        assert!(
            tokio::time::timeout(Duration::from_secs(2), revoked_task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(revoked_responses.recv().await.is_none());
        kept_tx
            .send(PeerMessage::Text(
                json!({"type":"request","id":2,"method":"GET","path":"/api/session"}).to_string(),
            ))
            .await
            .unwrap();
        assert!(kept_responses.recv().await.is_some());
        drop(kept_tx);
        kept_task.await.unwrap().unwrap();
        assert!(
            same_browser
                .dispatch("GET", "/api/access", vec![], None)
                .await
                .is_ok()
        );
        assert!(
            revoked
                .dispatch("POST", "/api/access/rotate", b"{}".to_vec(), None)
                .await
                .is_err()
        );
        assert!(
            revoked
                .dispatch("GET", "/api/state", vec![], None)
                .await
                .is_err()
        );
        assert!(shared.lock().await.known(&a));
        assert!(!shared.lock().await.known(&b));
    }
    #[test]
    fn strict_allowlist() {
        for (method, path) in [
            ("GET", "/api/session"),
            ("GET", "/api/state"),
            ("GET", "/api/commands?terminal=other%2Ft1"),
            ("POST", "/api/rpc"),
            ("POST", "/api/topology"),
            ("GET", "/api/component-updates"),
            ("POST", "/api/component-updates/check"),
            ("POST", "/api/component-updates/install"),
        ] {
            assert!(allowed(method, path));
        }
        for path in [
            "http://evil/api/state",
            "//evil/api/state",
            "/api/state?x=1",
            "/api/login",
            "/api/logout",
            "/api/events",
            "/api/terminal",
            "/api/attachments",
            "/api/update/install",
            "/api/component-updates?on=other",
            "/api/component-updates/install?url=evil",
            "/api/commands?terminal=t1&x=2",
            "/api/../api/state",
        ] {
            assert!(!allowed("GET", path));
            assert!(!allowed("POST", path));
        }
        assert!(!allowed("DELETE", "/api/state"));
    }
    #[tokio::test]
    async fn private_auth_and_native_method_allowlist() {
        let service = PeerService::new(Core::new("/nonexistent".into()));
        assert!(!service.app.auth.authorized(&HeaderMap::new()));
        assert!(service.app.auth.authorized(&service.headers));
        assert!(service.app.auth.same_origin(&service.headers, true).is_ok());
        let response = service
            .dispatch("GET", "/api/session", vec![], None)
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let response = service
            .dispatch(
                "POST",
                "/api/rpc",
                br#"{"method":"terminal.attach"}"#.to_vec(),
                Some("application/json"),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        let other = PeerService::new(service.app.core.clone());
        assert!(!other.app.auth.authorized(&service.headers));
    }
    #[tokio::test]
    async fn terminal_preserves_credit_full_frames_and_cancellation() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("core.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (acknowledged, ack) = tokio::sync::oneshot::channel();
        let native = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = BufReader::new(socket);
            let mut hello = String::new();
            socket.read_line(&mut hello).await.unwrap();
            let hello: Value = serde_json::from_str(&hello).unwrap();
            assert_eq!(hello["method"], "hello");
            socket
                .get_mut()
                .write_all(b"{\"id\":1,\"result\":{\"generation\":1,\"framed\":true}}\n")
                .await
                .unwrap();
            let mut header = [0u8; 9];
            socket.read_exact(&mut header).await.unwrap();
            let mut attach = vec![0; u32::from_le_bytes(header[5..].try_into().unwrap()) as usize];
            socket.read_exact(&mut attach).await.unwrap();
            let attach: Value = serde_json::from_slice(&attach).unwrap();
            assert_eq!(attach["method"], "terminal.attach");
            assert_eq!(attach["params"]["terminal"], "t1");
            let reply = br#"{"id":2,"result":{"channel":42,"terminal":{"address":"t1"}}}"#;
            let mut frame = vec![0, 0, 0, 0, 0];
            frame.extend((reply.len() as u32).to_le_bytes());
            frame.extend(reply);
            socket.get_mut().write_all(&frame).await.unwrap();
            let mut output = 42u32.to_le_bytes().to_vec();
            output.push(0);
            output.extend(262144u32.to_le_bytes());
            output.extend(vec![b'x'; 262144]);
            socket.get_mut().write_all(&output).await.unwrap();
            // The initial browser credit does NOT issue a second native grant.
            socket.read_exact(&mut header).await.unwrap();
            assert_eq!(header[4], 0);
            let mut input = vec![0; u32::from_le_bytes(header[5..].try_into().unwrap()) as usize];
            socket.read_exact(&mut input).await.unwrap();
            assert_eq!(input, b"input");
            socket.read_exact(&mut header).await.unwrap();
            assert_eq!(header[4], 1);
            let mut credit = [0; 4];
            socket.read_exact(&mut credit).await.unwrap();
            assert_eq!(u32::from_le_bytes(credit), 262144);
            acknowledged.send(()).unwrap();
            // Aborting the channel must release both halves of the Core socket.
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        });
        let service = PeerService::new(Core::new(path));
        let (tx, rx) = mpsc::channel(4);
        let (out, mut receive) = mpsc::channel(4);
        tx.send(PeerMessage::Text(json!({"terminal":"t1"}).to_string()))
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            service
                .serve_channel("herdr-terminal-v1:test", rx, out)
                .await
        });
        assert!(matches!(receive.recv().await, Some(PeerMessage::Text(_))));
        tx.send(PeerMessage::Text(
            json!({"type":"credit","bytes":262144}).to_string(),
        ))
        .await
        .unwrap();
        let Some(PeerMessage::Binary(output)) = receive.recv().await else {
            panic!("expected native output");
        };
        assert_eq!(output, vec![b'x'; 262144]);
        tx.send(PeerMessage::Binary(b"input".to_vec()))
            .await
            .unwrap();
        tx.send(PeerMessage::Text(
            json!({"type":"credit","bytes":262144}).to_string(),
        ))
        .await
        .unwrap();
        // Wait until the native side observed credit before cancellation.
        tokio::time::timeout(Duration::from_secs(2), ack)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(2), native)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn control_close_drops_subscriptions() {
        let service = PeerService::new(Core::new("/nonexistent".into()));
        let (tx, rx) = mpsc::channel(2);
        let (out, mut receive) = mpsc::channel(4);
        tx.send(PeerMessage::Text(
            json!({"type":"subscribe","id":1,"path":"/api/events"}).to_string(),
        ))
        .await
        .unwrap();
        let child = service.clone();
        let task =
            tokio::spawn(async move { child.serve_channel("herdr-control-v1", rx, out).await });
        assert!(matches!(receive.recv().await, Some(PeerMessage::Text(_))));
        drop(tx);
        task.await.unwrap().unwrap();
        // JoinSet cancellation releases subscription permits and native resources.
        tokio::task::yield_now().await;
        assert_eq!(service.subscriptions.available_permits(), 32);
    }
}
