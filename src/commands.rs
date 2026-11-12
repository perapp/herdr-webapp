//! Live Pi catalogs live in native versioned documents, never session files.
use crate::{
    Shared,
    auth::{ApiError, error},
};
use axum::{
    Json,
    extract::{Query, Request, State},
    http::{StatusCode, header},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: usize = 128 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandQuery {
    terminal: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Command {
    name: String,
    description: String,
    source: Source,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Source {
    Extension,
    Template,
    Skill,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Catalog {
    schema_version: u32,
    terminal: String,
    terminal_created_ms: u64,
    terminal_pid: u32,
    pid: u32,
    session_id: String,
    updated_at: u64,
    commands: Vec<Command>,
}
#[derive(Debug, PartialEq)]
struct Identity {
    address: String,
    created: u64,
    root: u32,
    foreground: u32,
}
fn local_id(s: &str) -> bool {
    s.strip_prefix('t').is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 13
            && s.bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
    })
}
fn address(s: &str) -> bool {
    if let Some((machine, id)) = s.split_once('/') {
        !machine.is_empty()
            && machine.len() <= 128
            && machine
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            && local_id(id)
    } else {
        local_id(s)
    }
}
fn identity(t: &Value, expected: &str) -> Option<Identity> {
    if t["address"].as_str()? != expected
        || t["state"] != "live"
        || t["status"]["app"] != "pi"
        || t["reported"] == false
    {
        return None;
    }
    let root = u32::try_from(t["pid"].as_u64()?).ok().filter(|p| *p > 0)?;
    // Native foreground is authoritative when present. Without it only a root Pi can publish.
    let foreground = if t["foreground"].is_object() {
        u32::try_from(t["foreground"]["pid"].as_u64()?)
            .ok()
            .filter(|p| *p > 0)?
    } else {
        root
    };
    Some(Identity {
        address: expected.into(),
        created: t["created_ms"].as_u64()?,
        root,
        foreground,
    })
}
fn safe_name(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{00ad}' | '\u{061c}' | '\u{06dd}' | '\u{070f}' | '\u{0890}'..='\u{0891}' | '\u{08e2}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}' | '\u{13430}'..='\u{1343f}' | '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0001}' | '\u{e0020}'..='\u{e007f}')
}
fn validate(value: Value, id: &Identity, local: &str, now: u64) -> Option<Vec<Command>> {
    if serde_json::to_vec(&value).ok()?.len() > MAX_BYTES {
        return None;
    }
    let catalog: Catalog = serde_json::from_value(value).ok()?;
    if catalog.schema_version != 1
        || catalog.terminal != local
        || catalog.terminal_created_ms != id.created
        || catalog.terminal_pid != id.root
        || catalog.pid != id.foreground
        || catalog.pid == 0
        || catalog.session_id.is_empty()
        || catalog.session_id.len() > 256
        || catalog.session_id.chars().any(unsafe_char)
        || catalog.updated_at == 0
        || now.saturating_sub(catalog.updated_at) > 75_000
        || catalog.updated_at.saturating_sub(now) > 5_000
        || catalog.commands.len() > 500
    {
        return None;
    }
    let mut seen = HashSet::new();
    let mut commands = catalog.commands;
    for command in &mut commands {
        if !safe_name(&command.name) || !seen.insert(command.name.clone()) {
            return None;
        }
        let mut units = 0;
        command.description = command
            .description
            .chars()
            .map(|c| if unsafe_char(c) { ' ' } else { c })
            .take_while(|c| {
                units += c.len_utf16();
                units <= 200
            })
            .collect();
    }
    Some(commands)
}
fn unavailable(message: &str) -> Json<Value> {
    Json(json!({"available":false,"message":message,"commands":[]}))
}

pub async fn handle(State(app): State<Shared>, request: Request) -> Result<Json<Value>, ApiError> {
    app.auth.require(request.headers())?;
    // Like state, ordinary authenticated GETs need no Origin; any supplied Origin is exact.
    if request.headers().contains_key(header::ORIGIN) {
        app.auth.same_origin(request.headers(), false)?;
    }
    let Query(query) = Query::<CommandQuery>::try_from_uri(request.uri()).map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "Supply only a selected terminal address",
        )
    })?;
    if !address(&query.terminal) {
        return Err(error(StatusCode::BAD_REQUEST, "Invalid terminal address"));
    }
    let Ok(first) = app
        .core
        .call("terminal.get", json!({"terminal":query.terminal}))
        .await
    else {
        return Ok(unavailable(
            "The selected terminal is unavailable. Reconnect and try again.",
        ));
    };
    let Some(id) = identity(&first, &query.terminal) else {
        return Ok(unavailable(
            "Live commands require Pi in the selected terminal.",
        ));
    };
    let (prefix, local) = query
        .terminal
        .split_once('/')
        .map_or((None, query.terminal.as_str()), |(p, l)| (Some(p), l));
    // Derive routing from native identity, not a client-supplied document or machine selector.
    let Ok(hello) = app.core.call("hello", json!({"generation":1})).await else {
        return Ok(unavailable("Command discovery is temporarily unavailable."));
    };
    let on = if let Some(prefix) = prefix {
        if hello["core"]["id"] == prefix || hello["core"]["name"] == prefix {
            None
        } else {
            let Ok(machines) = app.core.call("machine.list", json!({})).await else {
                return Ok(unavailable("Command discovery is temporarily unavailable."));
            };
            let configured = machines["machines"]
                .as_array()
                .and_then(|ms| ms.iter().find(|m| m["id"] == prefix || m["name"] == prefix));
            let Some(name) = configured.and_then(|m| m["name"].as_str()) else {
                return Ok(unavailable(
                    "Live commands are unsupported for inbound-only peers.",
                ));
            };
            Some(name.to_owned())
        }
    } else {
        None
    };
    let name = format!("herdr-pi-commands-{local}");
    if name.len() > 32 {
        return Ok(unavailable(
            "This terminal identity does not support command discovery.",
        ));
    }
    let Ok(doc) = app.core.call("doc.get", json!({"name":name,"on":on})).await else {
        return Ok(unavailable(
            "Live command discovery is unavailable. Install or reload the Pi command publisher.",
        ));
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let Some(commands) = validate(doc["value"].clone(), &id, local, now) else {
        return Ok(unavailable(
            "No fresh live commands. Install or reload the Pi command publisher, then reopen this palette.",
        ));
    };
    let Ok(current) = app
        .core
        .call("terminal.get", json!({"terminal":query.terminal}))
        .await
    else {
        return Ok(unavailable(
            "The selected terminal changed. Reopen the command palette.",
        ));
    };
    if identity(&current, &query.terminal).as_ref() != Some(&id) {
        return Ok(unavailable(
            "The selected terminal changed. Reopen the command palette.",
        ));
    }
    Ok(Json(
        json!({"available":true,"message":"Live commands from the current Pi session.","commands":commands}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{App, auth::Auth, core::Core};
    use axum::{body::Body, http::Request};
    use std::sync::Arc;
    use tower::ServiceExt;
    fn terminal() -> Value {
        json!({"address":"t3h","state":"live","pid":20,"created_ms":100,"reported":true,"status":{"app":"pi"},"foreground":{"pid":21}})
    }
    fn catalog() -> Value {
        json!({"schemaVersion":1,"terminal":"t3h","terminalCreatedMs":100,"terminalPid":20,"pid":21,"sessionId":"session","updatedAt":100_000,"commands":[{"name":"skill:hello","description":"hello\nworld","source":"skill"}]})
    }
    #[test]
    fn validation_binds_identity_freshness_and_limits() {
        let id = identity(&terminal(), "t3h").unwrap();
        let commands = validate(catalog(), &id, "t3h", 100_000).unwrap();
        assert_eq!(commands[0].description, "hello world");
        for (key, value) in [
            ("schemaVersion", json!(2)),
            ("terminal", json!("t4")),
            ("terminalCreatedMs", json!(101)),
            ("terminalPid", json!(22)),
            ("pid", json!(20)),
            ("sessionId", json!("")),
            ("updatedAt", json!(24_999)),
            ("updatedAt", json!(105_001)),
        ] {
            let mut c = catalog();
            c[key] = value;
            assert!(validate(c, &id, "t3h", 100_000).is_none(), "{key}");
        }
        for name in ["/foo", "-foo", "foo bar", "a\n", ""] {
            let mut c = catalog();
            c["commands"][0]["name"] = json!(name);
            assert!(validate(c, &id, "t3h", 100_000).is_none());
        }
        let mut c = catalog();
        c["commands"][0]["source"] = json!("prompt");
        assert!(validate(c, &id, "t3h", 100_000).is_none());
        let mut c = catalog();
        c["commands"] = json!(vec![c["commands"][0].clone(); 501]);
        assert!(validate(c, &id, "t3h", 100_000).is_none());
        let mut c = catalog();
        c["commands"][0]["description"] = json!("x".repeat(MAX_BYTES));
        assert!(validate(c, &id, "t3h", 100_000).is_none());
        let mut t = terminal();
        t["foreground"]["pid"] = json!(22);
        assert_ne!(identity(&t, "t3h"), Some(id));
        assert!(address("box/t3h"));
        assert!(!address("box/../t3h"));
        assert!(!address("reviewer"));
    }
    #[tokio::test]
    async fn offline_route_auth_origin_and_queries() {
        let app = Arc::new(App {
            updates: crate::update::Updater::development(),
            core: Core::new("/nonexistent".into()),
            auth: Auth::new("a".repeat(64), None),
        });
        let router = crate::router(app);
        for (uri, authorized, origin, expected) in [
            ("/api/commands?terminal=t3h", false, None, 401),
            (
                "/api/commands?terminal=t3h",
                true,
                Some("http://foreign.example"),
                403,
            ),
            ("/api/commands?terminal=t3h&path=secret", true, None, 400),
            ("/api/commands?terminal=t3h&on=box", true, None, 400),
            ("/api/commands?terminal=t3h&terminal=t4", true, None, 400),
            ("/api/commands?terminal=../x", true, None, 400),
            ("/api/commands", true, None, 400),
            ("/api/commands?terminal=t3h", true, None, 200),
            (
                "/api/commands?terminal=t3h",
                true,
                Some("http://localhost:8001"),
                200,
            ),
        ] {
            let mut request = Request::get(uri).header("host", "localhost:8001");
            if authorized {
                request = request.header("authorization", format!("Bearer {}", "a".repeat(64)));
            }
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), expected, "{uri}");
        }
    }
}
