//! Shared machine topology edits. JSON is edited in place, never round-tripped
//! through the desktop structs (which would discard unknown root/node fields).
use crate::{
    Shared,
    auth::{ApiError, error},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    action: String,
    on: Option<String>,
    space: Option<String>,
    tab: Option<String>,
    pane: Option<String>,
    name: Option<String>,
    folder: Option<String>,
    direction: Option<String>,
    version: u64,
}
fn bad(message: impl Into<String>) -> ApiError {
    error(StatusCode::BAD_REQUEST, message)
}
fn unsupported(message: impl Into<String>) -> ApiError {
    error(StatusCode::UNPROCESSABLE_ENTITY, message)
}
fn rpc_error(e: anyhow::Error) -> ApiError {
    // Core::call preserves the JSON RPC error as its display string.
    let message = e.to_string();
    let conflict = serde_json::from_str::<Value>(&message)
        .ok()
        .is_some_and(|v| v.pointer("/data/kind").and_then(Value::as_str) == Some("conflict"));
    error(
        if conflict {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_GATEWAY
        },
        message,
    )
}
fn check_version(result: &Value, expected: u64) -> Result<(), ApiError> {
    let actual = result["version"]
        .as_u64()
        .ok_or_else(|| unsupported("doc.get returned no valid version"))?;
    if actual != expected {
        return Err(error(
            StatusCode::CONFLICT,
            format!(
                "Layout changed: requested version {expected}, current {actual}; refresh before retrying"
            ),
        ));
    }
    if result.get("value").is_none() {
        return Err(unsupported("doc.get returned no value"));
    }
    Ok(())
}

pub async fn handle(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Request>,
) -> Result<Json<Value>, ApiError> {
    app.auth.require(&headers)?;
    app.auth.same_origin(&headers, false)?;
    body.validate()?;
    let read = app
        .core
        .call("doc.get", json!({"name":"herdr-app","on":body.on}))
        .await
        .map_err(rpc_error)?;
    check_version(&read, body.version)?;
    let mut doc = if read["value"].is_null() {
        json!({"spaces":[]})
    } else {
        read["value"].clone()
    };
    validate_document(&doc)?;
    // Plan against a placeholder first: missing targets, invalid geometry and
    // unsupported schemas must fail BEFORE a program is started.
    let mut plan = Plan::new(&body);
    let mut preview = doc.clone();
    apply(&mut preview, &body, &plan, "topology-preview")?;
    validate_document(&preview)?;
    if preview.to_string().len() > 1024 * 1024 {
        return Err(unsupported(
            "Edited layout exceeds Core's 1 MiB document limit",
        ));
    }
    let mut terminal = None;
    if plan.creates_terminal {
        let cwd = body
            .folder
            .as_deref()
            .or_else(|| {
                spaces(&doc)
                    .iter()
                    .find(|s| s["id"].as_str() == body.space.as_deref())
                    .and_then(|s| s["folder"].as_str())
            })
            .unwrap_or("");
        // No grouping labels here! Desktop reconcile adopts labeled orphans and
        // can create a conflicting unnamed tab before our CAS reaches core.
        let run = app.core.call("terminal.run", json!({
            "on":body.on,"cwd":cwd,"no_parent":true,"no_notify":true,"keep":true,
            "env":{"TERM":"xterm-256color","TERM_PROGRAM":"herdr-webapp","COLORTERM":"truecolor"}
        })).await.map_err(|e| error(StatusCode::BAD_GATEWAY, format!("terminal.run failed; outcome may be unknown. No automatic retry or cleanup: {e}")))?;
        let address = run["address"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| error(StatusCode::BAD_GATEWAY,"terminal.run returned no address; a terminal may be orphaned. Refresh terminals; no automatic cleanup"))?.to_owned();
        let local = local_terminal(&address, body.on.as_deref()).map_err(|e| {
            error(
                e.0,
                format!("{}; orphan terminal {address}; not killed", e.1),
            )
        })?;
        terminal = Some(address);
        plan.terminal = Some(local);
    }
    // Preview already checked the exact edit. Any subsequent failure must name
    // the possibly orphaned program; never kill it after an ambiguous write.
    let outcome = async {
        apply(
            &mut doc,
            &body,
            &plan,
            plan.terminal.as_deref().unwrap_or("unused"),
        )?;
        validate_document(&doc)?;
        let written = app
            .core
            .call(
                "doc.set",
                json!({"name":"herdr-app","on":body.on,"version":body.version,"value":doc}),
            )
            .await
            .map_err(rpc_error)?;
        written["version"].as_u64().ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "doc.set returned no version; write outcome unknown",
            )
        })
    }
    .await;
    let version = outcome.map_err(|ApiError(status, message)| {
        error(status, match &terminal { Some(t) => format!("{message}; possible orphan terminal {t} (on {:?}); left running, not killed. Refresh document and terminals before retrying", body.on), None => message })
    })?;
    let mut reply = json!({"on":body.on,"version":version,"value":doc,"space":plan.space,"tab":plan.tab,"pane":plan.pane,"terminal":terminal});
    if let Some(address) = terminal {
        // keep:true is required for desktop's lost-program shell replacement.
        // Labels remain useful for later reconciliation, but only after commit.
        // Private desktop active/focused/view documents are never changed.
        if let Err(e) = app
            .core
            .call(
                "terminal.label",
                json!({"terminal":address,"labels":{
                    "herdr.space":plan.space,"herdr.tab":plan.tab,"herdr.seen":"0"
                }}),
            )
            .await
        {
            reply["warning"] = format!("Layout committed, but terminal labels failed: {e}. Do not repeat the creation; refresh first").into();
        }
    }
    Ok(Json(reply))
}

impl Request {
    fn validate(&self) -> Result<(), ApiError> {
        if !matches!(
            self.action.as_str(),
            "rename-space" | "rename-tab" | "new-space" | "new-tab" | "new-pane" | "split"
        ) {
            return Err(bad("Unknown topology action"));
        }
        for id in [
            self.on.as_deref(),
            self.space.as_deref(),
            self.tab.as_deref(),
            self.pane.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !valid_id(id) {
                return Err(bad(
                    "Machine and topology IDs must be nonempty, machine-local, and contain no slash or control characters",
                ));
            }
        }
        if self.action != "new-space" && self.space.is_none() {
            return Err(bad("space is required"));
        }
        if matches!(self.action.as_str(), "rename-tab" | "new-pane" | "split") && self.tab.is_none()
        {
            return Err(bad("tab is required"));
        }
        if self.action == "split" && self.pane.is_none() {
            return Err(bad("pane is required"));
        }
        if self.action.starts_with("rename-") && self.name.is_none() {
            return Err(bad("name is required"));
        }
        if self
            .name
            .as_ref()
            .is_some_and(|n| n.len() > 4096 || n.chars().any(char::is_control))
        {
            return Err(bad("Invalid name"));
        }
        if self
            .folder
            .as_ref()
            .is_some_and(|n| n.len() > 32768 || n.contains('\0'))
        {
            return Err(bad("Invalid folder"));
        }
        if self
            .direction
            .as_deref()
            .is_some_and(|s| !matches!(s, "right" | "down"))
        {
            return Err(bad("direction must be right or down"));
        }
        if self.action != "split" && self.direction.is_some() {
            return Err(bad("direction is only valid for split"));
        }
        if self.action != "new-space" && self.action != "new-tab" && self.folder.is_some() {
            return Err(bad("folder is only valid for new-space or new-tab"));
        }
        Ok(())
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id.contains('/')
        && !id.chars().any(|c| c.is_control() || c.is_whitespace())
}
fn fresh(prefix: &str) -> String {
    format!("{prefix}{}", uuid::Uuid::new_v4().simple())
}
struct Plan {
    creates_terminal: bool,
    space: String,
    tab: String,
    pane: String,
    container: String,
    terminal: Option<String>,
}
impl Plan {
    fn new(r: &Request) -> Self {
        Self {
            creates_terminal: matches!(
                r.action.as_str(),
                "new-space" | "new-tab" | "new-pane" | "split"
            ),
            space: if r.action == "new-space" {
                fresh("sp")
            } else {
                r.space.clone().unwrap_or_default()
            },
            tab: if matches!(r.action.as_str(), "new-space" | "new-tab") {
                fresh("tb")
            } else {
                r.tab.clone().unwrap_or_default()
            },
            pane: fresh("p"),
            container: fresh("n"),
            terminal: None,
        }
    }
}
fn local_terminal(address: &str, on: Option<&str>) -> Result<String, ApiError> {
    let local = match on {
        Some(machine) => address
            .strip_prefix(&format!("{machine}/"))
            .unwrap_or(address),
        None => address,
    };
    if !valid_id(local) {
        return Err(unsupported(
            "Core returned a terminal address for a different machine",
        ));
    }
    Ok(local.to_owned())
}
fn spaces(doc: &Value) -> &[Value] {
    doc.get("spaces")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn apply(doc: &mut Value, r: &Request, p: &Plan, terminal: &str) -> Result<(), ApiError> {
    let pane = json!({"id":p.pane,"type":"block","content":{"terminal":terminal}});
    let tab = json!({"id":p.tab,"name":if r.action=="new-tab" {r.name.as_deref().unwrap_or("")} else {""},"root":{"id":p.tab,"type":"row","children":[pane.clone()]}});
    if !doc.as_object().unwrap().contains_key("spaces") {
        doc["spaces"] = json!([]);
    }
    let ss = doc["spaces"].as_array_mut().unwrap();
    if r.action == "new-space" {
        ss.push(json!({"id":p.space,"name":r.name.as_deref().unwrap_or(""),"folder":r.folder.as_deref().unwrap_or(""),"tabs":[tab]}));
        return Ok(());
    }
    let space = ss
        .iter_mut()
        .find(|s| s["id"] == p.space)
        .ok_or_else(|| bad("Space not found on this machine"))?;
    if r.action == "rename-space" {
        space["name"] = r.name.clone().unwrap().into();
        return Ok(());
    }
    if space.get("tabs").is_none() {
        space["tabs"] = json!([]);
    }
    let tabs = space["tabs"].as_array_mut().unwrap();
    if r.action == "new-tab" {
        tabs.push(tab);
        return Ok(());
    }
    let tab = tabs
        .iter_mut()
        .find(|t| t["id"] == p.tab)
        .ok_or_else(|| bad("Tab not found in requested space"))?;
    if r.action == "rename-tab" {
        tab["name"] = r.name.clone().unwrap().into();
        return Ok(());
    }
    if r.action == "new-pane" {
        if tab.get("root").is_none() {
            tab["root"] = json!({"id":p.container,"type":"row","children":[pane]});
        } else {
            let root = &mut tab["root"];
            if matches!(root["type"].as_str(), Some("row" | "column")) {
                root["children"].as_array_mut().unwrap().push(pane);
            } else {
                // Keep blocks/stacks and opaque fields intact inside their new slot.
                let mut old = root.take();
                let size = old.as_object_mut().unwrap().remove("size");
                *root = json!({"id":p.container,"type":"row","children":[old,pane]});
                if let Some(size) = size {
                    root["size"] = size;
                }
            }
        }
        return Ok(());
    }
    let root = tab
        .get_mut("root")
        .ok_or_else(|| bad("Tab has no pane to split"))?;
    if !split(
        root,
        r.pane.as_deref().unwrap(),
        if r.direction.as_deref() == Some("down") {
            "column"
        } else {
            "row"
        },
        &pane,
        &p.container,
    ) {
        return Err(bad(
            "Pane not found or is not a terminal/mod block in requested tab",
        ));
    }
    Ok(())
}
fn splittable(node: &Value) -> bool {
    node["type"] == "block"
        && (node["content"].get("terminal").is_some() || node["content"].get("mod").is_some())
}
/// Desktop Node::split geometry: reuse same-axis parent and halve the target
/// weight, otherwise wrap and move the slot size onto the new container.
fn split(node: &mut Value, target: &str, axis: &str, new: &Value, container: &str) -> bool {
    if node["type"] == axis
        && let Some(children) = node.get_mut("children").and_then(Value::as_array_mut)
        && let Some(i) = children
            .iter()
            .position(|c| c["id"] == target && splittable(c))
    {
        let share = children[i]
            .pointer("/size/weight")
            .and_then(Value::as_f64)
            .unwrap_or(1.0);
        if children[i].pointer("/size/px").is_none() {
            // Preserve extensions inside a size object too.
            if children[i].get("size").is_none_or(Value::is_null) {
                children[i]["size"] = json!({});
            }
            children[i]["size"]["weight"] = json!(share / 2.0);
        }
        let mut new = new.clone();
        new["size"] = json!({"weight":share/2.0});
        children.insert(i + 1, new);
        return true;
    }
    if node["id"] == target && splittable(node) {
        let mut old = node.take();
        let size = old.as_object_mut().unwrap().remove("size");
        *node = json!({"id":container,"type":axis,"children":[old,new]});
        if let Some(size) = size {
            node["size"] = size;
        }
        // A stack active reference names the old child. Since its identity now
        // sits inside a wrapper, update that reference in the caller below.
        return true;
    }
    let active = node
        .get("active")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
        for child in children {
            let direct = child["id"] == target;
            if split(child, target, axis, new, container) {
                if direct && active.as_deref() == Some(target) {
                    node["active"] = container.into();
                }
                return true;
            }
        }
    }
    false
}

// Current desktop documents are unversioned. Preserve arbitrary extension
// fields, but never silently edit an explicitly versioned/new structural kind.
fn schema(v: &Value) -> Result<(), ApiError> {
    if ["schema", "$schema", "schema_version", "schemaVersion"]
        .iter()
        .any(|k| v.get(k).is_some())
    {
        return Err(unsupported(
            "Explicit layout schema is unsupported; current herdr-app documents are unversioned",
        ));
    }
    Ok(())
}
fn object(v: &Value) -> Result<(), ApiError> {
    if v.is_object() {
        Ok(())
    } else {
        Err(unsupported("Layout object expected"))
    }
}
fn optional_string(v: &Value, key: &str) -> Result<(), ApiError> {
    if v.get(key).is_some_and(|v| !v.is_string()) {
        return Err(unsupported(format!("Invalid layout {key}")));
    }
    Ok(())
}
fn identity(v: &Value, ids: &mut HashSet<String>) -> Result<(), ApiError> {
    let id = v["id"]
        .as_str()
        .filter(|id| valid_id(id))
        .ok_or_else(|| unsupported("Invalid/missing machine-local layout ID"))?;
    if !ids.insert(id.to_owned()) {
        return Err(unsupported(format!("Duplicate layout ID {id}")));
    }
    Ok(())
}
fn validate_document(doc: &Value) -> Result<(), ApiError> {
    object(doc)?;
    schema(doc)?;
    let mut spaces_ids = HashSet::new();
    let mut tabs_ids = HashSet::new();
    let mut nodes_ids = HashSet::new();
    if let Some(ss) = doc.get("spaces") {
        for s in ss
            .as_array()
            .ok_or_else(|| unsupported("spaces must be an array"))?
        {
            object(s)?;
            schema(s)?;
            identity(s, &mut spaces_ids)?;
            optional_string(s, "name")?;
            optional_string(s, "folder")?;
            if let Some(tabs) = s.get("tabs") {
                for tab in tabs
                    .as_array()
                    .ok_or_else(|| unsupported("tabs must be an array"))?
                {
                    object(tab)?;
                    schema(tab)?;
                    identity(tab, &mut tabs_ids)?;
                    optional_string(tab, "name")?;
                    if let Some(root) = tab.get("root") {
                        validate_node(root, &mut nodes_ids, 0)?;
                    }
                }
            }
        }
    }
    Ok(())
}
fn positive(v: &Value) -> bool {
    v.as_f64().is_some_and(|n| n.is_finite() && n > 0.0)
}
fn validate_content(v: &Value) -> Result<(), ApiError> {
    object(v)?;
    let variants = ["terminal", "mod", "tab", "context", "agent"];
    let keys: Vec<_> = variants.iter().filter(|k| v.get(**k).is_some()).collect();
    if keys.len() != 1 {
        return Err(unsupported("Unknown or ambiguous layout content"));
    }
    let key = *keys[0];
    let name = v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| unsupported("Invalid layout content reference"))?;
    if key == "terminal" && !valid_id(name) {
        return Err(unsupported("Terminal reference must be machine-local"));
    }
    if let Some(frame) = v.get("pane").filter(|x| !x.is_null()) {
        validate_style(frame)?;
    }
    Ok(())
}
/// Validate known rendering fields too, without serializing them or interpreting
/// extension fields. An invalid authored style must not be written back as if
/// this client understood the document.
fn validate_style(v: &Value) -> Result<(), ApiError> {
    object(v)?;
    for key in [
        "font",
        "ui_font",
        "shadow",
        "preview",
        "terminal_menu",
        "toast",
        "title",
        "reveal",
        "position",
    ] {
        if v.get(key).is_some_and(|x| !x.is_null()) {
            optional_string(v, key)?;
        }
    }
    for key in [
        "radius",
        "gap",
        "height",
        "font_size",
        "terminal_line_height",
        "terminal_scroll_speed",
        "terminal_opacity",
    ] {
        if v.get(key)
            .is_some_and(|x| !x.is_null() && !x.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0))
        {
            return Err(unsupported(format!("Invalid style {key}")));
        }
    }
    if v.get("lone").is_some_and(|x| !x.is_boolean()) {
        return Err(unsupported("Invalid pane lone flag"));
    }
    if let Some(theme) = v.get("theme").filter(|x| !x.is_null())
        && !theme.is_string()
    {
        object(theme)?;
        if !theme["light"].is_string() || !theme["dark"].is_string() {
            return Err(unsupported("Invalid theme choice"));
        }
    }
    if let Some(colors) = v.get("colors").filter(|x| !x.is_null()) {
        object(colors)?;
        if colors.as_object().unwrap().values().any(|c| !c.is_string()) {
            return Err(unsupported("Invalid theme colors"));
        }
    }
    for key in ["terminal_padding", "traffic_lights"] {
        if let Some(x) = v.get(key).filter(|x| !x.is_null()) {
            validate_pair(x)?;
        }
    }
    if let Some(x) = v.get("padding").filter(|x| !x.is_null())
        && !x.is_number()
    {
        validate_pair(x)?;
    }
    if let Some(x) = v.get("titlebar").filter(|x| !x.is_null())
        && !matches!(x.as_str(), Some("system" | "none"))
    {
        return Err(unsupported("Unsupported titlebar"));
    }
    for key in ["border", "divider", "focus"] {
        if let Some(line) = v.get(key).filter(|x| !x.is_null()) {
            object(line)?;
            if key == "focus" && (line.get("border").is_some() || line.get("background").is_some())
            {
                if line
                    .get("background")
                    .is_some_and(|x| !x.is_null() && !x.is_string())
                {
                    return Err(unsupported("Invalid focus background"));
                }
                if let Some(border) = line.get("border").filter(|x| !x.is_null()) {
                    validate_line(border)?;
                }
            } else {
                validate_line(line)?;
            }
        }
    }
    if let Some(bg) = v.get("background").filter(|x| !x.is_null())
        && !bg.is_string()
    {
        object(bg)?;
        if let Some(g) = bg.get("gradient") {
            if !g
                .as_array()
                .is_some_and(|a| a.len() == 2 && a.iter().all(Value::is_string))
                || bg.get("angle").is_some_and(|a| !a.is_number())
            {
                return Err(unsupported("Invalid gradient"));
            }
        } else if bg["image"].is_string() {
            if bg
                .get("fit")
                .is_some_and(|x| !matches!(x.as_str(), Some("cover" | "contain")))
            {
                return Err(unsupported("Unsupported image fit"));
            }
        } else {
            return Err(unsupported("Unsupported background"));
        }
    }
    if let Some(c) = v.get("cloned_from").filter(|x| !x.is_null()) {
        object(c)?;
        if !c["name"].is_string() || !c["version"].is_string() {
            return Err(unsupported("Invalid cloned_from"));
        }
    }
    if let Some(header) = v.get("header").filter(|x| !x.is_null()) {
        validate_style(header)?;
        let c = &header["content"];
        if !c.is_string() && !c.get("mod").is_some_and(Value::is_string) {
            return Err(unsupported("Unsupported header content"));
        }
    }
    Ok(())
}
fn validate_line(v: &Value) -> Result<(), ApiError> {
    object(v)?;
    if v.get("width")
        .is_some_and(|x| !x.as_f64().is_some_and(|n| n >= 0.0))
        || v.get("color").is_some_and(|x| !x.is_string())
    {
        return Err(unsupported("Invalid layout line"));
    }
    Ok(())
}
fn validate_pair(v: &Value) -> Result<(), ApiError> {
    if !v
        .as_array()
        .is_some_and(|a| a.len() == 2 && a.iter().all(Value::is_number))
    {
        return Err(unsupported("Expected numeric pair"));
    }
    Ok(())
}
fn validate_node(v: &Value, ids: &mut HashSet<String>, depth: usize) -> Result<(), ApiError> {
    if depth > 64 {
        return Err(unsupported("Layout is too deep"));
    }
    object(v)?;
    schema(v)?;
    identity(v, ids)?;
    validate_style(v)?;
    for key in [
        "title",
        "font",
        "ui_font",
        "shadow",
        "preview",
        "terminal_menu",
        "toast",
    ] {
        if v.get(key).is_some_and(|x| !x.is_null()) {
            optional_string(v, key)?;
        }
    }
    for key in ["hidden", "window_drag", "movable", "closable", "resizable"] {
        if v.get(key).is_some_and(|x| {
            !(x.is_boolean() || x.is_null() && matches!(key, "movable" | "closable" | "resizable"))
        }) {
            return Err(unsupported(format!("Invalid layout {key}")));
        }
    }
    if let Some(size) = v.get("size").filter(|x| !x.is_null()) {
        object(size)?;
        let px = size.get("px");
        let weight = size.get("weight");
        if px.is_some() == weight.is_some() || !px.or(weight).is_some_and(positive) {
            return Err(unsupported("Invalid layout size"));
        }
    }
    for key in [
        "min",
        "max",
        "radius",
        "gap",
        "font_size",
        "terminal_line_height",
        "terminal_scroll_speed",
        "terminal_opacity",
    ] {
        if v.get(key)
            .is_some_and(|x| !x.is_null() && !x.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0))
        {
            return Err(unsupported(format!("Invalid layout {key}")));
        }
    }
    if let (Some(min), Some(max)) = (v["min"].as_f64(), v["max"].as_f64())
        && min > max
    {
        return Err(unsupported("Layout min exceeds max"));
    }
    match v["type"].as_str() {
        Some("block") => {
            if v.get("children").is_some() {
                return Err(unsupported("Block cannot have children"));
            }
            validate_content(&v["content"])?;
        }
        Some("row" | "column" | "stack") => {
            if v.get("content").is_some() {
                return Err(unsupported("Container cannot have block content"));
            }
            let children = v["children"]
                .as_array()
                .ok_or_else(|| unsupported("Container children must be an array"))?;
            for c in children {
                validate_node(c, ids, depth + 1)?;
            }
            if let Some(active) = v.get("active").filter(|x| !x.is_null())
                && (v["type"] != "stack"
                    || !active.is_string()
                    || !children.iter().any(|c| c["id"] == *active))
            {
                return Err(unsupported("Invalid stack active child reference"));
            }
        }
        _ => return Err(unsupported("Unsupported future layout node type")),
    }
    if let Some(floats) = v.get("floating") {
        for f in floats
            .as_array()
            .ok_or_else(|| unsupported("floating must be an array"))?
        {
            object(f)?;
            identity(f, ids)?;
            validate_content(&f["content"])?;
            if f.get("edge")
                .is_some_and(|e| !matches!(e.as_str(), Some("top" | "right" | "bottom" | "left")))
                || f.get("align")
                    .is_some_and(|e| !matches!(e.as_str(), Some("start" | "center" | "end")))
            {
                return Err(unsupported("Invalid floating geometry"));
            }
            if f.get("hidden").is_some_and(|x| !x.is_boolean())
                || f.get("peek").is_some_and(|x| !x.is_null() && !positive(x))
            {
                return Err(unsupported("Invalid floating visibility/peek"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn doc() -> Value {
        json!({"extension":{"retain":[1,2]},"spaces":[{"id":"s","name":"old","folder":"/w","color":"teal","tabs":[{"id":"t","name":"old tab","future":42,"root":{"id":"t","type":"row","extra":"root","children":[{"id":"p","type":"block","size":{"weight":0.6,"extra":true},"unknown":[1],"content":{"terminal":"t1","extra":"keep"}},{"id":"q","type":"block","content":{"mod":"viewer","props":{"a":1}}}]}}]}]})
    }
    fn request(action: &str) -> Request {
        serde_json::from_value(json!({"action":action,"on":null,"space":"s","tab":"t","pane":"p","name":"renamed","version":4})).unwrap()
    }
    #[test]
    fn new_pane_appends_to_requested_tab_without_changing_existing_nodes() {
        for axis in ["row", "column"] {
            let mut d = doc();
            d["spaces"][0]["tabs"][0]["root"]["type"] = axis.into();
            let before = d.clone();
            let mut r = request("new-pane");
            r.pane = None;
            r.validate().unwrap();
            let p = Plan::new(&r);
            assert!(p.creates_terminal);
            apply(&mut d, &r, &p, "t9").unwrap();
            validate_document(&d).unwrap();
            assert_eq!(d["extension"], before["extension"]);
            let children = d["spaces"][0]["tabs"][0]["root"]["children"]
                .as_array()
                .unwrap();
            assert_eq!(children.len(), 3);
            assert_eq!(
                children[0],
                before["spaces"][0]["tabs"][0]["root"]["children"][0]
            );
            assert_eq!(
                children[1],
                before["spaces"][0]["tabs"][0]["root"]["children"][1]
            );
            assert_eq!(children[2]["content"]["terminal"], "t9");
        }
    }
    #[test]
    fn new_pane_handles_empty_tabs_and_preserves_stack_roots() {
        let mut d = doc();
        d["spaces"][0]["tabs"][0]["root"]["children"] = json!([]);
        let r = request("new-pane");
        let p = Plan::new(&r);
        apply(&mut d, &r, &p, "t9").unwrap();
        validate_document(&d).unwrap();
        assert_eq!(
            d["spaces"][0]["tabs"][0]["root"]["children"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let mut d = doc();
        let root = &mut d["spaces"][0]["tabs"][0]["root"];
        root["type"] = "stack".into();
        root["active"] = "q".into();
        root["size"] = json!({"px":200});
        let before = root.clone();
        apply(&mut d, &r, &p, "t9").unwrap();
        validate_document(&d).unwrap();
        let root = &d["spaces"][0]["tabs"][0]["root"];
        assert_eq!(root["type"], "row");
        assert_eq!(root["size"], before["size"]);
        assert_eq!(root["children"][0]["active"], "q");
        assert_eq!(root["children"][0]["children"], before["children"]);
        assert_eq!(root["children"][0]["extra"], before["extra"]);
        assert_eq!(root["children"][1]["content"]["terminal"], "t9");
        let mut missing_tab = request("new-pane");
        missing_tab.tab = None;
        assert!(missing_tab.validate().is_err());
    }
    #[test]
    fn creation_appends_without_changing_existing_layout_or_view() {
        for action in ["new-space", "new-tab"] {
            let mut d = doc();
            let before = d.clone();
            let r = request(action);
            let p = Plan::new(&r);
            apply(&mut d, &r, &p, "t9").unwrap();
            validate_document(&d).unwrap();
            assert_eq!(d["extension"], before["extension"]);
            let added = if action == "new-space" {
                assert_eq!(d["spaces"][0], before["spaces"][0]);
                assert_eq!(d["spaces"][1]["name"], "renamed");
                &d["spaces"][1]["tabs"][0]
            } else {
                assert_eq!(d["spaces"][0]["tabs"][0], before["spaces"][0]["tabs"][0]);
                assert_eq!(d["spaces"][0]["tabs"][1]["name"], "renamed");
                &d["spaces"][0]["tabs"][1]
            };
            assert_eq!(added["root"]["children"][0]["content"]["terminal"], "t9");
            assert!(added.get("focused").is_none());
            assert!(d.get("active").is_none());
        }
        assert_eq!(local_terminal("remote/t9", Some("remote")).unwrap(), "t9");
        assert!(local_terminal("other/t9", Some("remote")).is_err());
    }
    #[test]
    fn rename_preserves_every_other_field() {
        for (action, path) in [
            ("rename-space", "/spaces/0/name"),
            ("rename-tab", "/spaces/0/tabs/0/name"),
        ] {
            let mut d = doc();
            let mut expected = d.clone();
            *expected.pointer_mut(path).unwrap() = json!("renamed");
            let r = request(action);
            apply(&mut d, &r, &Plan::new(&r), "unused").unwrap();
            validate_document(&d).unwrap();
            assert_eq!(d, expected);
        }
    }
    #[test]
    fn split_same_axis_halves_share_and_preserves_extensions() {
        let mut d = doc();
        let r = request("split");
        let p = Plan::new(&r);
        apply(&mut d, &r, &p, "t2").unwrap();
        validate_document(&d).unwrap();
        let c = &d["spaces"][0]["tabs"][0]["root"]["children"];
        assert_eq!(c[0]["size"], json!({"weight":0.3,"extra":true}));
        assert_eq!(c[1]["size"]["weight"], 0.3);
        assert_eq!(c[2]["id"], "q");
        assert_eq!(
            c[0]["content"],
            doc()["spaces"][0]["tabs"][0]["root"]["children"][0]["content"]
        );
        assert_eq!(d["extension"], doc()["extension"]);
    }
    #[test]
    fn split_wrap_moves_size_and_updates_stack_reference() {
        let mut n = json!({"id":"stack","type":"stack","active":"p","children":[{"id":"p","type":"block","size":{"px":200},"extra":1,"content":{"terminal":"t1"}}]});
        assert!(split(
            &mut n,
            "p",
            "column",
            &json!({"id":"new","type":"block","content":{"terminal":"t2"}}),
            "wrap"
        ));
        assert_eq!(n["active"], "wrap");
        assert_eq!(n["children"][0]["size"]["px"], 200);
        assert!(n["children"][0]["children"][0].get("size").is_none());
        assert_eq!(n["children"][0]["children"][0]["extra"], 1);
        validate_node(&n, &mut HashSet::new(), 0).unwrap();
    }
    #[test]
    fn invalid_layout_and_future_schema_refused() {
        for path in [
            "/spaces/0/tabs/0/root/type",
            "/spaces/0/tabs/0/root/children/1/id",
        ] {
            let mut d = doc();
            *d.pointer_mut(path).unwrap() = json!(if path.ends_with("type") {
                "future-kind"
            } else {
                "p"
            });
            assert!(validate_document(&d).is_err());
        }
        let mut d = doc();
        d["schema_version"] = json!(2);
        assert!(validate_document(&d).is_err());
        let mut n = json!({"id":"s","type":"stack","active":"missing","children":[]});
        assert!(validate_node(&n, &mut HashSet::new(), 0).is_err());
        n["active"] = Value::Null;
        assert!(validate_node(&n, &mut HashSet::new(), 0).is_ok());
    }
    #[test]
    fn versions_are_strict() {
        assert!(check_version(&json!({"value":null,"version":4}), 4).is_ok());
        assert_eq!(
            check_version(&json!({"value":null,"version":5}), 4)
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );
        assert!(check_version(&json!({"value":null}), 0).is_err());
        assert_eq!(
            rpc_error(anyhow::anyhow!("{}", json!({"data":{"kind":"conflict"}}))).0,
            StatusCode::CONFLICT
        );
    }
    #[tokio::test]
    async fn cas_conflict_leaves_terminal_unlabeled_and_alive() {
        use tokio::{
            io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
            net::UnixListener,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("core.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (io, _) = listener.accept().await.unwrap();
            let mut io = BufReader::new(io);
            for method in ["hello", "doc.get", "terminal.run", "doc.set"] {
                let mut line = String::new();
                io.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(req["method"], method);
                let result = match method {
                    "hello" => json!({"generation":1}),
                    "doc.get" => json!({"version":4,"value":doc()}),
                    "terminal.run" => {
                        assert_eq!(req["params"]["keep"], true);
                        assert_eq!(req["params"]["no_parent"], true);
                        assert!(req["params"].get("labels").is_none());
                        json!({"address":"t2"})
                    }
                    _ => {
                        assert_eq!(req["params"]["version"], 4);
                        json!(null)
                    }
                };
                let response = if method == "doc.set" {
                    json!({"id":req["id"],"error":{"data":{"kind":"conflict"},"message":"changed"}})
                } else {
                    json!({"id":req["id"],"result":result})
                };
                io.get_mut()
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
            }
            let mut line = String::new();
            match tokio::time::timeout(
                std::time::Duration::from_millis(100),
                io.read_line(&mut line),
            )
            .await
            {
                Err(_) | Ok(Ok(0)) => {} // Core drops the connection on RPC errors.
                other => {
                    panic!("must not retry, label, or kill after CAS conflict: {other:?} {line}")
                }
            }
        });
        let app = std::sync::Arc::new(crate::App {
            core: crate::core::Core::new(path),
            auth: crate::auth::Auth::new("a".repeat(64), None),
        });
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", "a".repeat(64)).parse().unwrap(),
        );
        let err = handle(State(app), headers, Json(request("new-tab")))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("orphan terminal t2"));
        server.await.unwrap();
    }
}
