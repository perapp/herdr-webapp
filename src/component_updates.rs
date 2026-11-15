//! Authenticated, per-connector update controls. Never updates the hosted frontend.
//! Core updates are manual, generation-pinned and delegated exclusively to hc.
use crate::{
    Shared,
    auth::{ApiError, error},
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::Mutex,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    pub state: String,
    pub current_version: Option<String>,
    pub available_version: Option<String>,
    pub available_revision: Option<String>,
    pub identity: Option<String>,
    pub channel: Option<String>,
    pub managed: Option<String>,
    pub instruction: Option<String>,
    pub error: Option<String>,
    pub can_check: bool,
    pub can_install: bool,
}
impl Component {
    fn unavailable(message: &str) -> Self {
        Self {
            state: "unavailable".into(),
            current_version: None,
            available_version: None,
            available_revision: None,
            identity: None,
            channel: None,
            managed: None,
            instruction: Some(message.into()),
            error: None,
            can_check: false,
            can_install: false,
        }
    }
    fn fail(&mut self, message: String) {
        self.state = "error".into();
        self.error = Some(message);
        self.can_install = false;
        self.can_check = true;
        self.identity = None;
        self.available_version = None;
        self.available_revision = None;
    }
}
#[derive(Clone)]
struct CoreCandidate {
    version: String,
    identity: String,
    bin: Option<PathBuf>,
    core_identity: Value,
}
struct Inner {
    core: Component,
    candidate: Option<CoreCandidate>,
    busy: bool,
}
pub struct Updates {
    inner: Mutex<Inner>,
    socket: Option<PathBuf>,
    connector_busy: Mutex<()>,
}
impl Updates {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                core: Component {
                    state: "unchecked".into(),
                    channel: Some("stable (protocol generation 1)".into()),
                    can_check: true,
                    ..Component::unavailable(
                        "Check to determine Core release availability and installation ownership. Updates are explicit and preserve terminals; clients reconnect after hc restarts Core.",
                    )
                },
                candidate: None,
                busy: false,
            }),
            socket: std::env::var_os("HERDR_CONNECTOR_CONTROL_SOCKET")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
            connector_busy: Mutex::new(()),
        })
    }
    async fn connector(&self, action: &str, identity: Option<&str>) -> Result<Component, String> {
        let Some(path) = &self.socket else {
            return Ok(Component {
                current_version: Some(crate::build_info::VERSION.into()),
                ..Component::unavailable(
                    "This connector is not supervised by the hosted launcher. Update it using its original installation method and relaunch; the hosted frontend has a separate deployment lifecycle.",
                )
            });
        };
        let result = tokio::time::timeout(Duration::from_secs(20), async {
            let mut stream = UnixStream::connect(path).await.map_err(|_| "Launcher control is unavailable; rerun the original launch command.".to_owned())?;
            let expected = identity.map(decode_connector_identity).transpose()?;
            let mut request = json!({"schemaVersion":1,"action":if action == "install" { "update" } else { action }});
            if let Some((version, revision)) = expected {
                request["expectedVersion"] = json!(version);
                if let Some(revision) = revision { request["expectedRevision"] = json!(revision); }
            }
            stream.write_all(format!("{request}\n").as_bytes()).await.map_err(|_| "Launcher request failed".to_owned())?;
            let mut bytes = Vec::new();
            use tokio::io::AsyncReadExt;
            BufReader::new(stream).take(16385).read_until(b'\n', &mut bytes).await.map_err(|_| "Launcher reply failed".to_owned())?;
            if bytes.len() > 16384 || bytes.last() != Some(&b'\n') { return Err("Invalid launcher response".into()); }
            let reply: LauncherReply = serde_json::from_slice(&bytes).map_err(|_| "Invalid launcher response".to_owned())?;
            translate_launcher(reply)
        }).await;
        result.unwrap_or_else(|_| Err("Launcher request timed out; refresh status before retrying. An install may still be running.".into()))
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LauncherReply {
    schema_version: u64,
    status: String,
    current_version: String,
    available_version: Option<String>,
    channel: String,
    message: Option<String>,
    available_revision: Option<String>,
}
fn decode_connector_identity(text: &str) -> Result<(String, Option<String>), String> {
    serde_json::from_str(text)
        .map_err(|_| "Invalid connector release identity; check again.".into())
}
fn translate_launcher(reply: LauncherReply) -> Result<Component, String> {
    if reply.schema_version != 1 || !["stable", "main"].contains(&reply.channel.as_str()) {
        return Err("Unsupported launcher protocol/channel".into());
    }
    let state = match reply.status.as_str() {
        "idle" => "unchecked",
        "failed" => "error",
        "checking" => "checking",
        "current" => "current",
        "available" => "available",
        "updating" => "updating",
        _ => return Err("Unknown launcher update state".into()),
    };
    let candidate = reply.available_version.as_ref().filter(|v| !v.is_empty());
    let identity = candidate
        .map(|version| serde_json::to_string(&(version, &reply.available_revision)).unwrap());
    let can_install = state == "available"
        && identity.is_some()
        && (reply.channel != "main"
            || reply
                .available_revision
                .as_ref()
                .is_some_and(|r| !r.is_empty()));
    Ok(Component { state: state.into(), current_version: Some(reply.current_version), available_version: reply.available_version,
        available_revision: reply.available_revision, identity, channel: Some(reply.channel), managed: Some("connector launcher".into()),
        instruction: Some("The launcher checks every 30 seconds. Install is explicit unless its operator enabled --auto-update. Updating the connector never updates shared Core or the separately deployed frontend.".into()),
        error: (state == "error").then_some(reply.message).flatten(), can_check: !matches!(state,"checking"|"updating"), can_install })
}
pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/component-updates", get(status))
        .route("/api/component-updates/check", post(check))
        .route("/api/component-updates/install", post(install))
}
fn authorize(app: &Shared, headers: &HeaderMap) -> Result<(), ApiError> {
    app.auth.require(headers)?;
    app.auth.same_origin(headers, false)
}
async fn snapshot(app: &Shared) -> Value {
    snapshot_with_connector(app, None).await
}
async fn snapshot_with_connector(app: &Shared, connector: Option<Component>) -> Value {
    let updates = &app.core.component_updates;
    let connector = match connector {
        Some(value) => value,
        None => updates
            .connector("status", None)
            .await
            .unwrap_or_else(|message| {
                let mut c = Component::unavailable(
                    "Reconnect to the launcher's original computer and retry.",
                );
                c.fail(message);
                c
            }),
    };
    // Use the persistent Core monitor, not a blocking RPC per status poll. In
    // particular a restarting Core must not hold up the connector's install ACK.
    let core_state = app.core.state.borrow().clone();
    let mut inner = updates.inner.lock().await;
    if let Some(version) = core_state["coreVersion"].as_str() {
        inner.core.current_version = Some(version.into());
    }
    if inner.core.state == "reconnecting"
        && core_state["connected"] == true
        && let Some(candidate) = &inner.candidate
        && core_state["coreVersion"]
            .as_str()
            .is_some_and(|v| version(v) >= version(&candidate.version))
    {
        inner.core.state = "current".into();
        inner.core.can_check = true;
        inner.core.available_version = None;
        inner.core.identity = None;
        inner.core.can_install = false;
        inner.candidate = None;
    }
    json!({"connector":connector,"core":inner.core,"computer":core_state["core"]["name"].as_str().map(|s| s.chars().take(128).collect::<String>()),
        "policy":{"checks":"manual Core checks; connector launcher checks every 30 seconds","coreChecks":"manual","connectorCheckSeconds":30,"automaticCoreUpdate":false,
        "frontend":"Hosted frontend updates are deployed by the server operator; this browser cannot replace the hosted service."}})
}
async fn status(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    // Same-origin browser GETs normally omit Origin; no mutation occurs here.
    app.auth.require(&headers)?;
    if headers.contains_key(axum::http::header::ORIGIN) {
        app.auth.same_origin(&headers, false)?;
    }
    Ok(Json(snapshot(&app).await))
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Connector,
    Core,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Check {
    component: Kind,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Install {
    component: Kind,
    expected_identity: String,
}
async fn check(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Check>,
) -> Result<Json<Value>, ApiError> {
    authorize(&app, &headers)?;
    let mut connector = None;
    match body.component {
        Kind::Connector => {
            connector = Some(connector_action(&app, "check", None).await?);
        }
        Kind::Core => {
            let updates = app.core.component_updates.clone();
            let mut inner = updates.inner.lock().await;
            if inner.busy || inner.core.state == "reconnecting" {
                return Err(error(
                    StatusCode::CONFLICT,
                    "A Core update operation is already running",
                ));
            }
            inner.busy = true;
            inner.core.state = "checking".into();
            inner.core.can_check = false;
            inner.core.error = None;
            inner.core.can_install = false;
            inner.candidate = None;
            drop(inner);
            let core = app.core.clone();
            tokio::spawn(async move {
                let result = check_core(&core).await;
                let mut inner = updates.inner.lock().await;
                inner.busy = false;
                match result {
                    Ok((component, candidate)) => {
                        inner.core = component;
                        inner.candidate = candidate;
                    }
                    Err(e) => inner.core.fail(e),
                }
            });
        }
    }
    Ok(Json(snapshot_with_connector(&app, connector).await))
}
async fn connector_action(
    app: &Shared,
    action: &str,
    identity: Option<&str>,
) -> Result<Component, ApiError> {
    let updates = &app.core.component_updates;
    // The guard drops even if a peer disconnects during the IPC request.
    let _guard = updates.connector_busy.try_lock().map_err(|_| {
        error(
            StatusCode::CONFLICT,
            "A connector operation is already running",
        )
    })?;
    updates
        .connector(action, identity)
        .await
        .map_err(|e| error(StatusCode::BAD_GATEWAY, e))
}
async fn install(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Install>,
) -> Result<Json<Value>, ApiError> {
    authorize(&app, &headers)?;
    if body.expected_identity.is_empty() || body.expected_identity.len() > 1024 {
        return Err(error(StatusCode::BAD_REQUEST, "Invalid update identity"));
    }
    let mut connector = None;
    match body.component {
        Kind::Connector => {
            connector =
                Some(connector_action(&app, "install", Some(&body.expected_identity)).await?)
        }
        Kind::Core => {
            let updates = app.core.component_updates.clone();
            let mut inner = updates.inner.lock().await;
            if inner.busy || inner.core.state == "reconnecting" {
                return Err(error(
                    StatusCode::CONFLICT,
                    "A Core operation is already running",
                ));
            }
            let candidate = inner
                .candidate
                .clone()
                .filter(|c| c.identity == body.expected_identity && inner.core.can_install)
                .ok_or_else(|| {
                    error(
                        StatusCode::CONFLICT,
                        "Check for a compatible update again before installing",
                    )
                })?;
            inner.busy = true;
            inner.core.state = "updating".into();
            inner.core.can_check = false;
            inner.core.can_install = false;
            inner.core.error = None;
            drop(inner);
            let core = app.core.clone();
            tokio::spawn(async move {
                let result = install_core(&core, &candidate).await;
                let mut inner = updates.inner.lock().await;
                inner.busy = false;
                match result {
                    Ok(()) => {
                        inner.core.state = "reconnecting".into();
                        inner.core.instruction = Some("Core updated; waiting for its connection to return. Terminals are retained by hc.".into());
                    }
                    Err(e) => {
                        inner.candidate = None;
                        inner.core.fail(e);
                    }
                }
            });
        }
    }
    Ok(Json(snapshot_with_connector(&app, connector).await))
}
fn managed_instruction(manager: &str) -> &'static str {
    match manager {
        "homebrew" => "Run brew upgrade herdr-core on this computer.",
        "mise" => "Run mise upgrade herdr-core on this computer.",
        "nix" => "Update herdr-core in this computer's Nix configuration.",
        _ => "Update herdr-core using the package manager or installation method that owns it.",
    }
}
fn version(text: &str) -> Option<[u64; 3]> {
    let mut parts = text.strip_prefix("hc ").unwrap_or(text).split('.');
    let number = |part: &str| -> Option<u64> {
        (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    };
    let nums = [
        number(parts.next()?)?,
        number(parts.next()?)?,
        number(parts.next()?)?,
    ];
    parts.next().is_none().then_some(nums)
}
#[derive(Deserialize)]
struct Release {
    version: String,
    generation: u64,
}
async fn check_core(
    core: &Arc<crate::core::Core>,
) -> Result<(Component, Option<CoreCandidate>), String> {
    let status = core
        .call("server.status", json!({}))
        .await
        .map_err(|_| "Core is disconnected or incompatible with protocol generation 1; reconnect or use a compatible Core and check again.".to_owned())?;
    {
        // Ownership is useful even offline; a failed release lookup must not hide
        // package-manager recovery instructions already reported by Core.
        let mut inner = core.component_updates.inner.lock().await;
        inner.core.current_version = status["version"].as_str().map(str::to_owned);
        inner.core.managed = status["installed_by"].as_str().map(str::to_owned);
        if let Some(manager) = inner.core.managed.as_deref() {
            inner.core.instruction = Some(managed_instruction(manager).into());
        }
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = client
        .get("https://releases.herdr.dev/core/latest.json")
        .send()
        .await
        .map_err(|_| {
            "Cannot check Core releases; check this computer's network connection and retry."
                .to_owned()
        })?
        .error_for_status()
        .map_err(|_| "Core release service is unavailable; retry later.".to_owned())?;
    if response.content_length().is_some_and(|n| n > 65536) {
        return Err("Core release metadata is too large".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Cannot read Core release metadata".to_owned())?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err("Core release metadata is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let release: Release = serde_json::from_slice(&bytes)
        .map_err(|_| "Invalid Core compatibility metadata".to_owned())?;
    let bin = verified_running_binary(core, &status).await;
    core_release(&status, release, bin)
}
fn core_release(
    status: &Value,
    release: Release,
    bin: Option<PathBuf>,
) -> Result<(Component, Option<CoreCandidate>), String> {
    let have = status["version"]
        .as_str()
        .ok_or("Core did not report a version")?;
    let have_version =
        version(have).ok_or("This Core build does not report a supported release version")?;
    let latest = version(&release.version).ok_or("Invalid Core release version")?;
    let managed = status["installed_by"].as_str().map(str::to_owned);
    let mut component = Component { state: "current".into(), current_version: Some(have.into()), available_version: None, available_revision: None, identity: None,
        channel: Some("stable (protocol generation 1)".into()), managed: managed.clone(), error: None, can_check: true, can_install: false,
        instruction: Some("No automatic Core updates. hc preserves terminals and clients reconnect after an explicit update.".into()) };
    if let Some(manager) = managed.as_deref() {
        component.instruction = Some(managed_instruction(manager).into());
    }
    if release.generation != 1 {
        component.state = "unavailable".into();
        component.instruction = Some(format!(
            "The latest Core release uses another protocol generation. No compatible update is offered; update your clients before changing generations. {}",
            managed
                .as_deref()
                .map(managed_instruction)
                .unwrap_or("No update is executed automatically.")
        ));
        return Ok((component, None));
    }
    if latest <= have_version {
        return Ok((component, None));
    }
    use sha2::{Digest, Sha256};
    let scope = serde_json::to_vec(&(status["core"]["id"].clone(), &release.version))
        .map_err(|_| "Invalid Core identity")?;
    let hash: String = Sha256::digest(scope)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let identity = format!("core:{hash}");
    component.state = "available".into();
    component.available_version = Some(release.version.clone());
    component.identity = Some(identity.clone());
    component.can_install = managed.is_none() && bin.is_some() && have_version >= [0, 1, 5];
    if managed.is_none() && !component.can_install {
        component.instruction = Some("In-app update execution is unavailable for this Core installation/platform. Run hc update on this computer (without --force); it refuses incompatible generations and preserves terminals.".into());
    }
    Ok((
        component,
        Some(CoreCandidate {
            version: release.version,
            identity,
            bin,
            core_identity: status["core"].clone(),
        }),
    ))
}
/// Resolve the running Core, never an unrelated hc on PATH. Unsupported platforms
/// offer the official manual command rather than updating a potentially different Core.
#[cfg(target_os = "linux")]
fn running_binary(status: &Value) -> Option<PathBuf> {
    let pid = status["pid"].as_u64()?;
    let path = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    path.is_absolute().then_some(path).filter(|p| p.is_file())
}
/// A forwarded Unix socket can report a remote PID that happens to exist here.
/// Match kernel peer credentials to server.status, not just its claimed PID.
async fn verified_running_binary(core: &Arc<crate::core::Core>, status: &Value) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let metadata = std::fs::metadata(&core.path).ok()?;
        // SAFETY: geteuid has no preconditions and only reads process identity.
        let uid = unsafe { libc::geteuid() };
        if !metadata.file_type().is_socket() || metadata.uid() != uid {
            return None;
        }
        let stream = tokio::time::timeout(Duration::from_secs(3), UnixStream::connect(&core.path))
            .await
            .ok()?
            .ok()?;
        let peer = stream.peer_cred().ok()?;
        if peer.uid() != uid
            || peer.pid().and_then(|p| u64::try_from(p).ok()) != status["pid"].as_u64()
        {
            return None;
        }
        running_binary(status)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (core, status);
        None
    }
}
async fn install_core(
    core: &Arc<crate::core::Core>,
    candidate: &CoreCandidate,
) -> Result<(), String> {
    let status = core
        .call("server.status", json!({}))
        .await
        .map_err(|_| "Core is disconnected; check again before retrying.".to_owned())?;
    if status["installed_by"].is_string() {
        return Err(managed_instruction(status["installed_by"].as_str().unwrap()).into());
    }
    if status["core"] != candidate.core_identity
        || verified_running_binary(core, &status).await != candidate.bin
    {
        return Err("Core installation changed; check again before updating.".into());
    }
    let bin = candidate
        .bin
        .as_ref()
        .ok_or("This Core installation requires a manual hc update")?;
    let probe = tokio::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let probe = tokio::time::timeout(Duration::from_secs(5), probe)
        .await
        .map_err(|_| "Core version probe timed out")?
        .map_err(|_| "Cannot execute this Core installation")?;
    if !probe.status.success()
        || String::from_utf8_lossy(&probe.stdout).trim()
            != format!("hc {}", status["version"].as_str().unwrap_or_default())
    {
        return Err(
            "Core executable does not match the selected running Core; check again.".into(),
        );
    }
    let output = tokio::process::Command::new(bin)
        .args(["update", "--json", "--version", &candidate.version])
        .env("HC_SOCKET", &core.path)
        .env_remove("HERDR_RELEASES_URL")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|_| "Cannot run hc update; check installation permissions.".to_owned())?;
    if !output.status.success() {
        let reply: Value = serde_json::from_slice(&output.stderr).unwrap_or(Value::Null);
        return Err(format!(
            "hc update failed: {} {}. Check again before retrying.",
            reply
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("see the launcher's logs or run hc update manually"),
            reply
                .pointer("/error/hint")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ));
    }
    let reply: Value = serde_json::from_slice(&output.stdout).map_err(|_| "hc update returned an unknown result; reconnect and check the actual version before retrying.")?;
    if reply["now"]
        .as_str()
        .and_then(version)
        .is_none_or(|v| Some(v) < version(&candidate.version))
    {
        return Err(
            "Core update did not confirm the requested version; reconnect and check again.".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_fail_closed() {
        assert_eq!(version("0.1.20"), Some([0, 1, 20]));
        for bad in [
            "0.1.2/evil",
            "0.1",
            "1.2.3.4",
            "0.1.20-rc",
            "https://host/0.1.2",
        ] {
            assert_eq!(version(bad), None);
        }
    }
    #[test]
    fn request_surface_is_fixed() {
        for bad in [
            r#"{"component":"core","url":"evil"}"#,
            r#"{"component":"shell"}"#,
            r#"{"component":"core","path":"/bin/sh"}"#,
        ] {
            assert!(serde_json::from_str::<Check>(bad).is_err());
        }
        assert!(
            serde_json::from_str::<Install>(
                r#"{"component":"core","expectedIdentity":"x","command":"evil"}"#
            )
            .is_err()
        );
    }
    #[test]
    fn package_managers_have_actionable_instructions() {
        for name in ["homebrew", "nix", "mise", "system package manager"] {
            assert!(!managed_instruction(name).is_empty());
        }
    }
    #[test]
    fn core_release_selection_never_crosses_generation_or_managed_ownership() {
        let status = json!({"version":"0.1.18","core":{"id":"test-core","name":"isolated"},"installed_by":null});
        let release = || Release {
            version: "0.1.20".into(),
            generation: 1,
        };
        let (c, candidate) = core_release(&status, release(), Some("/fake/hc".into())).unwrap();
        assert_eq!(c.state, "available");
        assert!(c.can_install);
        assert!(candidate.is_some());
        let (c, candidate) = core_release(
            &status,
            Release {
                version: "0.2.0".into(),
                generation: 2,
            },
            Some("/fake/hc".into()),
        )
        .unwrap();
        assert_eq!(c.state, "unavailable");
        assert!(!c.can_install);
        assert!(candidate.is_none());
        for manager in ["homebrew", "nix", "mise", "system package manager"] {
            let mut status = status.clone();
            status["installed_by"] = json!(manager);
            let (c, _) = core_release(&status, release(), Some("/fake/hc".into())).unwrap();
            assert_eq!(c.state, "available");
            assert!(!c.can_install);
            assert_eq!(c.instruction.as_deref(), Some(managed_instruction(manager)));
        }
        let (c, _) = core_release(&status, release(), None).unwrap();
        assert!(!c.can_install);
        let mut newer = status.clone();
        newer["version"] = json!("0.1.21");
        let (c, candidate) = core_release(&newer, release(), Some("/fake/hc".into())).unwrap();
        assert_eq!(c.state, "current");
        assert!(candidate.is_none());
    }
    #[test]
    fn main_identity_includes_revision_and_never_claims_a_release() {
        let reply = |revision| LauncherReply {
            schema_version: 1,
            status: "available".into(),
            current_version: "261115120000+0100-1234567".into(),
            available_version: Some("0.0.8".into()),
            channel: "main".into(),
            message: None,
            available_revision: revision,
        };
        let c = translate_launcher(reply(Some("abcdef0123456789".into()))).unwrap();
        assert!(c.can_install);
        assert_eq!(c.available_revision.as_deref(), Some("abcdef0123456789"));
        assert_eq!(
            decode_connector_identity(c.identity.as_deref().unwrap()).unwrap(),
            ("0.0.8".into(), Some("abcdef0123456789".into()))
        );
        assert!(!translate_launcher(reply(None)).unwrap().can_install);
    }
    #[tokio::test]
    async fn launcher_ipc_is_fixed_and_scoped() {
        use tokio::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            for action in ["status", "check", "update"] {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["schemaVersion"], 1);
                assert_eq!(request["action"], action);
                assert!(request.get("url").is_none());
                assert!(request.get("path").is_none());
                if action == "update" {
                    assert_eq!(request["expectedVersion"], "0.0.8");
                    assert_eq!(request["expectedRevision"], "abcdef0");
                } else {
                    assert!(request.get("expectedVersion").is_none());
                }
                reader.get_mut().write_all(b"{\"schemaVersion\":1,\"status\":\"available\",\"currentVersion\":\"build-1234567\",\"availableVersion\":\"0.0.8\",\"channel\":\"main\",\"message\":null,\"availableRevision\":\"abcdef0\"}\n").await.unwrap();
            }
        });
        let mut updates = Updates::new();
        Arc::get_mut(&mut updates).unwrap().socket = Some(socket);
        for action in ["status", "check", "install"] {
            let c = updates
                .connector(
                    action,
                    if action == "install" {
                        Some(r#"["0.0.8","abcdef0"]"#)
                    } else {
                        None
                    },
                )
                .await
                .unwrap();
            assert_eq!(c.channel.as_deref(), Some("main"));
            assert!(c.can_install);
        }
        server.await.unwrap();
    }
    #[tokio::test]
    async fn duplicate_and_stale_core_operations_are_rejected_without_execution() {
        let app = Arc::new(crate::App {
            core: crate::core::Core::new("/nonexistent".into()),
            auth: crate::auth::Auth::new("test-token".into(), None),
            updates: crate::update::Updater::development(),
        });
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer test-token".parse().unwrap());
        app.core.component_updates.inner.lock().await.busy = true;
        let result = check(
            State(app.clone()),
            headers.clone(),
            Json(Check {
                component: Kind::Core,
            }),
        )
        .await;
        assert!(result.is_err());
        let result = install(
            State(app.clone()),
            headers.clone(),
            Json(Install {
                component: Kind::Core,
                expected_identity: "stale".into(),
            }),
        )
        .await;
        assert!(result.is_err());
        app.core.component_updates.inner.lock().await.busy = false;
        let result = install(
            State(app.clone()),
            headers,
            Json(Install {
                component: Kind::Core,
                expected_identity: "stale".into(),
            }),
        )
        .await;
        assert!(result.is_err());
        assert!(!app.core.component_updates.inner.lock().await.busy);
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn selected_core_must_match_kernel_socket_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("core.sock");
        let _listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let core = crate::core::Core::new(socket);
        let status = json!({"pid":std::process::id()});
        assert_eq!(
            verified_running_binary(&core, &status).await,
            Some(std::env::current_exe().unwrap())
        );
        let remote = json!({"pid":std::process::id() + 1000});
        assert!(verified_running_binary(&core, &remote).await.is_none());
        let core = crate::core::Core::new(dir.path().join("absent"));
        assert!(verified_running_binary(&core, &status).await.is_none());
    }
    #[test]
    fn failures_clear_stale_install_capabilities() {
        let mut component = Component {
            state: "available".into(),
            available_version: Some("0.1.20".into()),
            identity: Some("stale".into()),
            can_install: true,
            ..Component::unavailable("retry")
        };
        component.fail("offline; retry".into());
        assert_eq!(component.state, "error");
        assert!(component.can_check);
        assert!(!component.can_install);
        assert!(component.identity.is_none());
        assert!(component.available_version.is_none());
        assert!(component.error.is_some());
    }
    #[tokio::test]
    async fn auth_and_origin_are_required() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        let app = Arc::new(crate::App {
            core: crate::core::Core::new("/nonexistent".into()),
            auth: crate::auth::Auth::new("test-token".into(), Some("http://example.test".into())),
            updates: crate::update::Updater::development(),
        });
        for (method, path, body) in [
            ("GET", "/api/component-updates", ""),
            (
                "POST",
                "/api/component-updates/check",
                r#"{"component":"core"}"#,
            ),
            (
                "POST",
                "/api/component-updates/install",
                r#"{"component":"core","expectedIdentity":"x"}"#,
            ),
        ] {
            let response = crate::router(app.clone())
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let response = crate::router(app.clone())
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("authorization", "Bearer test-token")
                        .header("origin", "http://evil.test")
                        .header("host", "example.test")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
}
