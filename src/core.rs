use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::{Mutex, Notify, watch},
};

pub struct Plain {
    pub io: BufReader<UnixStream>,
    next: u64,
    pub hello: Value,
}
impl Plain {
    pub async fn connect(path: &PathBuf, framed: bool) -> Result<Self> {
        let stream =
            tokio::time::timeout(Duration::from_secs(10), UnixStream::connect(path)).await??;
        let mut conn = Self {
            io: BufReader::new(stream),
            next: 0,
            hello: Value::Null,
        };
        conn.hello = tokio::time::timeout(
            Duration::from_secs(10),
            conn.call("hello", json!({"generation":1,"framed":framed})),
        )
        .await
        .context("core handshake timed out")??;
        if conn.hello["generation"] != 1 {
            bail!("unsupported core protocol generation");
        }
        if framed && conn.hello["framed"] != true {
            bail!("core did not accept framed mode");
        }
        Ok(conn)
    }
    pub async fn line(&mut self) -> Result<Value> {
        use tokio::io::AsyncReadExt;
        let mut line = Vec::new();
        let n = (&mut self.io)
            .take(16 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if n == 0 {
            bail!("core disconnected");
        }
        if n > 16 * 1024 * 1024 || line.last() != Some(&b'\n') {
            bail!("core response exceeds limit");
        }
        Ok(serde_json::from_slice(&line)?)
    }
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        let bytes = format!(
            "{}\n",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        );
        self.io.get_mut().write_all(bytes.as_bytes()).await?;
        loop {
            let msg = self.line().await?;
            if msg["id"] != id {
                continue;
            }
            if let Some(err) = msg.get("error") {
                bail!("{}", err);
            }
            return msg.get("result").cloned().context("missing RPC result");
        }
    }
}

pub struct Core {
    pub path: PathBuf,
    pub component_updates: Arc<crate::component_updates::Updates>,
    rpc: Mutex<Option<Plain>>,
    pub state: watch::Sender<Value>,
    metadata: Notify,
    metadata_epoch: std::sync::atomic::AtomicU64,
    pub notifications: std::sync::OnceLock<Arc<crate::notifications::Notifications>>,
    pub notification_events: tokio::sync::broadcast::Sender<crate::notifications::Observation>,
}
impl Core {
    pub fn new(path: PathBuf) -> Arc<Self> {
        let (state, _) = watch::channel(
            json!({"connected":false,"metadataReady":false,"error":"Connecting to Herdr Core…","terminals":[],"machines":[],"documents":[]}),
        );
        Arc::new(Self {
            path,
            component_updates: crate::component_updates::Updates::new(),
            rpc: Mutex::new(None),
            state,
            metadata: Notify::new(),
            metadata_epoch: std::sync::atomic::AtomicU64::new(0),
            notifications: std::sync::OnceLock::new(),
            notification_events: tokio::sync::broadcast::channel(1024).0,
        })
    }
    /// A single persistent, serialized RPC connection. Never retry mutations after an ambiguous failure.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut slot = self.rpc.lock().await;
        if slot.is_none() {
            *slot = Some(Plain::connect(&self.path, false).await?);
        }
        let r = tokio::time::timeout(
            Duration::from_secs(30),
            slot.as_mut().unwrap().call(method, params),
        )
        .await;
        match r {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => {
                *slot = None;
                Err(e)
            }
            Err(_) => {
                *slot = None;
                bail!("core request timed out; outcome may be unknown, refresh before retrying");
            }
        }
    }
    async fn documents(&self, machines: &Value) -> Vec<Value> {
        let mut names = vec![None];
        if let Some(machines) = machines.as_array() {
            for machine in machines {
                // MachineView flattens the configured machine together with connection status.
                if let Some(name) = machine["name"]
                    .as_str()
                    .or_else(|| machine["machine"]["name"].as_str())
                {
                    names.push(Some(name.to_owned()));
                }
            }
        }
        let mut docs = Vec::new();
        for on in names {
            let item = match self
                .call("doc.get", json!({"name":"herdr-app","on":on}))
                .await
            {
                Ok(v) => json!({"on":on,"version":v["version"],"value":v["value"]}),
                Err(e) => json!({"on":on,"error":e.to_string(),"version":0,"value":null}),
            };
            docs.push(item);
        }
        docs
    }
    async fn refresh_metadata(&self) -> Result<()> {
        let epoch = self
            .metadata_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        let machines = self.call("machine.list", json!({})).await?["machines"].clone();
        let documents = self.documents(&machines).await;
        self.state.send_modify(|s| {
            // A refresh begun before reconnect cannot make the new session ready.
            if s["connected"] != true
                || self
                    .metadata_epoch
                    .load(std::sync::atomic::Ordering::Acquire)
                    != epoch
            {
                return;
            }
            s["machines"] = machines;
            s["documents"] = json!(documents);
            s["metadataReady"] = true.into();
        });
        Ok(())
    }
    pub async fn monitor(self: Arc<Self>) {
        let metadata = self.clone();
        tokio::spawn(async move {
            loop {
                metadata.metadata.notified().await;
                if metadata.state.borrow()["connected"] == true {
                    let _ = metadata.refresh_metadata().await;
                }
            }
        });
        let mut backoff = 1;
        loop {
            let outcome = self.subscribe().await;
            let established = self.state.borrow()["connected"] == true;
            if established {
                backoff = 1;
            }
            self.state.send_modify(|s| {
                s["connected"] = false.into();
                s["error"] = outcome
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "Core disconnected".into())
                    .into();
            });
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(15);
        }
    }
    fn publish_notification_snapshot(&self) {
        // Only the monitor publishes observations, so resync snapshots are ordered
        // after all preceding facts and before any subsequent live transition.
        if let Some(service) = self.notifications.get() {
            service
                .needs_snapshot
                .swap(false, std::sync::atomic::Ordering::AcqRel);
        }
        let records = self.state.borrow()["terminals"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let _ = self
            .notification_events
            .send(crate::notifications::Observation::Snapshot(records));
    }
    fn publish_notification_message(&self, message: &Value, advanced: bool) {
        let resync = self.notifications.get().is_some_and(|service| {
            service
                .needs_snapshot
                .load(std::sync::atomic::Ordering::Acquire)
        });
        if message["method"] == "missed" || resync {
            // State already incorporates this message: seed it, do not replay it.
            self.publish_notification_snapshot();
        } else if message["method"] == "event" && advanced {
            let params = &message["params"];
            if params["type"] == "removed" {
                if let Some(address) = params["terminal"].as_str() {
                    let _ = self
                        .notification_events
                        .send(crate::notifications::Observation::Removed(address.into()));
                }
            } else if let Some(record) = self.state.borrow()["terminals"]
                .as_array()
                .and_then(|ts| ts.iter().find(|t| t["address"] == params["terminal"]))
            {
                let _ = self
                    .notification_events
                    .send(crate::notifications::Observation::Event {
                        kind: params["type"].as_str().unwrap_or("").into(),
                        record: record.clone(),
                    });
            }
        }
    }
    async fn subscribe(&self) -> Result<()> {
        let mut events = Plain::connect(&self.path, false).await?;
        // A successful fresh hello supersedes the last-known version, even if
        // this older Core omits it. Disconnects alone retain the last known value.
        self.state.send_modify(|s| apply_hello(s, &events.hello));
        let snapshot = tokio::time::timeout(
            Duration::from_secs(30),
            events.call("events.subscribe", json!({})),
        )
        .await
        .context("core subscription timed out")??;
        let mut seq = snapshot["seq"].as_u64().unwrap_or(0);
        self.state.send_modify(|s| {
            self.metadata_epoch
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            s["terminals"] = snapshot["terminals"].clone();
            s["metadataReady"] = false.into();
            s["connected"] = true.into();
            s["error"] = Value::Null;
        });
        self.publish_notification_snapshot();
        self.metadata.notify_one();
        loop {
            let message = tokio::time::timeout(Duration::from_secs(45), events.line())
                .await
                .context("core heartbeat timed out")??;
            let mut metadata = false;
            let previous_seq = seq;
            self.state
                .send_modify(|s| metadata = notification(s, &message, &mut seq));
            self.publish_notification_message(&message, seq > previous_seq);
            if metadata {
                self.metadata.notify_one();
            }
        }
    }
}

fn apply_hello(state: &mut Value, hello: &Value) {
    state["core"] = hello["core"].clone();
    let version = hello["version"].as_str().filter(|s| {
        !s.is_empty()
            && s.len() <= 128
            && s.trim() == *s
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
    });
    if let Some(version) = version {
        state["coreVersion"] = json!(version);
    } else if let Some(object) = state.as_object_mut() {
        object.remove("coreVersion");
    }
}

/// A missed backlog replaces the whole terminal snapshot and its watermark.
/// Old queued facts must not resurrect terminals removed by that snapshot.
fn notification(state: &mut Value, message: &Value, seq: &mut u64) -> bool {
    let p = &message["params"];
    match message["method"].as_str() {
        Some("missed") => {
            if let (Some(next), Some(terminals)) = (p["seq"].as_u64(), p["terminals"].as_array()) {
                *seq = next;
                state["terminals"] = json!(terminals);
            }
            true
        }
        Some("event") => {
            let next = p["seq"].as_u64().unwrap_or(0);
            if next <= *seq {
                return false;
            }
            *seq = next;
            if p["type"] == "doc" {
                true
            } else {
                reduce_event(state, p);
                false
            }
        }
        Some("heartbeat") => true,
        _ => false,
    }
}

/// Apply facts in stream order. No screen reads and no request per event.
fn reduce_event(state: &mut Value, event: &Value) {
    let (Some(address), Some(kind)) = (event["terminal"].as_str(), event["type"].as_str()) else {
        return;
    };
    let data = &event["data"];
    let Some(terminals) = state["terminals"].as_array_mut() else {
        return;
    };
    if kind == "removed" {
        terminals.retain(|t| t["address"] != address);
        return;
    }
    let replacement = if matches!(kind, "created" | "restarted") {
        Some(data)
    } else {
        data.get("record")
    };
    if let Some(record) = replacement {
        if let Some(t) = terminals.iter_mut().find(|t| t["address"] == address) {
            *t = record.clone();
        } else {
            terminals.push(record.clone());
        }
        return;
    }
    let Some(record) = terminals.iter_mut().find(|t| t["address"] == address) else {
        return;
    };
    let keys: &[&str] = match kind {
        "status" => &["status", "records", "reported"],
        "title" => &["title"],
        "turn" => &["turn"],
        "labels" => &["labels"],
        "keep" => &["keep"],
        "size" => &["cols", "rows"],
        "finished" | "lost" => &["reason", "exit_code", "signal", "finished_ms"],
        "foreground" => {
            record["foreground"] = data.clone();
            return;
        }
        _ => &[],
    };
    for key in keys {
        record[*key] = data[*key].clone();
    }
    if matches!(kind, "finished" | "lost") {
        record["state"] = if kind == "lost" { "live" } else { "finished" }.into();
        record["foreground"] = Value::Null;
        if kind == "lost" {
            record["pid"] = Value::Null;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notification_resync_on_heartbeat_is_ordered_and_removals_are_forwarded() {
        use crate::notifications::{Notifications, Observation};
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let service = Notifications::open(&dir.path().join("push")).unwrap();
        let core = Core::new("/nonexistent".into());
        core.notifications.set(service.clone()).ok().unwrap();
        let mut events = core.notification_events.subscribe();
        core.state
            .send_modify(|s| s["terminals"] = json!([{"address":"t1","status":{"state":"done"}}]));
        service.needs_snapshot.store(true, Ordering::Release);
        core.publish_notification_message(&json!({"method":"heartbeat"}), false);
        assert!(!service.needs_snapshot.load(Ordering::Acquire));
        assert!(
            matches!(events.try_recv().unwrap(), Observation::Snapshot(records) if records[0]["status"]["state"] == "done")
        );
        // A resync triggered by a live event seeds its result, not a live replay.
        service.needs_snapshot.store(true, Ordering::Release);
        let status = json!({"method":"event","params":{"type":"status","terminal":"t1"}});
        core.publish_notification_message(&status, true);
        assert!(matches!(
            events.try_recv().unwrap(),
            Observation::Snapshot(_)
        ));
        assert!(events.try_recv().is_err());
        core.publish_notification_message(&status, true);
        assert!(matches!(
            events.try_recv().unwrap(),
            Observation::Event { .. }
        ));
        core.publish_notification_message(
            &json!({"method":"event","params":{"type":"removed","terminal":"t1"}}),
            true,
        );
        assert!(
            matches!(events.try_recv().unwrap(), Observation::Removed(address) if address == "t1")
        );
    }
    #[tokio::test]
    async fn metadata_ready_only_after_documents_complete_and_not_for_stale_refresh() {
        for stale in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("core.sock");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let core = Core::new(path);
            assert_eq!(core.state.borrow()["metadataReady"], false);
            core.state.send_modify(|s| s["connected"] = true.into());
            let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (io, _) = listener.accept().await.unwrap();
                let mut io = BufReader::new(io);
                for result in [json!({"generation":1}), json!({"machines":[]})] {
                    let mut line = String::new();
                    io.read_line(&mut line).await.unwrap();
                    let request: Value = serde_json::from_str(&line).unwrap();
                    io.get_mut()
                        .write_all(
                            format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes(),
                        )
                        .await
                        .unwrap();
                }
                let mut line = String::new();
                io.read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], "doc.get");
                arrived_tx.send(()).unwrap();
                release_rx.await.unwrap();
                io.get_mut()
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"id":request["id"],"result":{"version":7,"value":{"tabs":[]}}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });
            let refreshing = core.clone();
            let refresh = tokio::spawn(async move { refreshing.refresh_metadata().await.unwrap() });
            arrived_rx.await.unwrap();
            assert_eq!(core.state.borrow()["metadataReady"], false);
            if stale {
                core.metadata_epoch
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            }
            release_tx.send(()).unwrap();
            refresh.await.unwrap();
            assert_eq!(core.state.borrow()["metadataReady"], !stale);
            if !stale {
                assert_eq!(core.state.borrow()["documents"][0]["version"], 7);
            }
            server.await.unwrap();
        }
    }
    #[test]
    fn hello_version_is_top_level_last_known_and_cleared_on_legacy_hello() {
        let mut state = json!({"connected":false});
        assert!(state.get("coreVersion").is_none());
        apply_hello(
            &mut state,
            &json!({"version":"0.1.42-dev+abc","core":{"id":"native"}}),
        );
        assert_eq!(state["coreVersion"], "0.1.42-dev+abc");
        assert!(state["core"].get("version").is_none());
        state["connected"] = json!(false);
        assert_eq!(state["coreVersion"], "0.1.42-dev+abc");
        for version in [
            json!(null),
            json!(42),
            json!(""),
            json!("bad\nvalue"),
            json!(" padded "),
            json!({"version":"1"}),
        ] {
            apply_hello(&mut state, &json!({"version":version,"core":{"id":"next"}}));
            assert!(state.get("coreVersion").is_none());
        }
        apply_hello(&mut state, &json!({"version":"1.0","core":{}}));
        apply_hello(&mut state, &json!({"core":{"version":"not-top-level"}}));
        assert!(state.get("coreVersion").is_none());
    }
    #[test]
    fn missed_replaces_snapshot_and_ignores_older_events() {
        let mut state = json!({"terminals":[{"address":"t1","title":"old"}]});
        let mut seq = 3;
        assert!(notification(
            &mut state,
            &json!({"method":"missed","params":{"seq":10,"terminals":[{"address":"beta/t2","title":"fresh"}]}}),
            &mut seq
        ));
        notification(
            &mut state,
            &json!({"method":"event","params":{"seq":8,"type":"created","terminal":"t1","data":{"address":"t1"}}}),
            &mut seq,
        );
        assert_eq!(state["terminals"].as_array().unwrap().len(), 1);
        assert_eq!(seq, 10);
        notification(
            &mut state,
            &json!({"method":"event","params":{"seq":11,"type":"title","terminal":"beta/t2","data":{"title":"current"}}}),
            &mut seq,
        );
        assert_eq!(state["terminals"][0]["title"], "current");
        assert_eq!(seq, 11);
    }
    #[test]
    fn facts_preserve_other_fields_and_remote_identity() {
        let mut state = json!({"terminals":[{"address":"beta/t1","title":"old","extra":7}]});
        reduce_event(
            &mut state,
            &json!({"type":"title","terminal":"beta/t1","data":{"title":"new"}}),
        );
        assert_eq!(state["terminals"][0]["title"], "new");
        assert_eq!(state["terminals"][0]["extra"], 7);
        reduce_event(
            &mut state,
            &json!({"type":"status","terminal":"beta/t1","data":{"status":{"state":"working"},"reported":true}}),
        );
        assert_eq!(state["terminals"][0]["status"]["state"], "working");
        reduce_event(
            &mut state,
            &json!({"type":"removed","terminal":"t1","data":{}}),
        );
        assert_eq!(state["terminals"].as_array().unwrap().len(), 1);
        reduce_event(
            &mut state,
            &json!({"type":"removed","terminal":"beta/t1","data":{}}),
        );
        assert!(state["terminals"].as_array().unwrap().is_empty());
    }
    #[tokio::test]
    async fn subscription_propagates_fresh_hello_version_and_clears_legacy_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("core.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            for hello in [
                json!({"generation":1,"version":"0.1.42","core":{"id":"first"}}),
                json!({"generation":1,"core":{"id":"legacy"}}),
            ] {
                let (io, _) = listener.accept().await.unwrap();
                let mut io = BufReader::new(io);
                for result in [hello, json!({"seq":0,"terminals":[]})] {
                    let mut line = String::new();
                    io.read_line(&mut line).await.unwrap();
                    let request: Value = serde_json::from_str(&line).unwrap();
                    io.get_mut()
                        .write_all(
                            format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes(),
                        )
                        .await
                        .unwrap();
                }
            }
        });
        let core = Core::new(path);
        assert!(core.subscribe().await.is_err());
        assert_eq!(core.state.borrow()["coreVersion"], "0.1.42");
        assert_eq!(core.state.borrow()["metadataReady"], false);
        core.state.send_modify(|s| s["metadataReady"] = true.into());
        // Same transition used by monitor on disconnect; no hello means no loss
        // of the last-known version.
        core.state.send_modify(|s| s["connected"] = json!(false));
        assert_eq!(core.state.borrow()["coreVersion"], "0.1.42");
        assert!(core.subscribe().await.is_err());
        assert!(core.state.borrow().get("coreVersion").is_none());
        assert_eq!(core.state.borrow()["metadataReady"], false);
        assert_eq!(core.state.borrow()["core"]["id"], "legacy");
        server.await.unwrap();
    }
    #[tokio::test]
    async fn observer_receives_live_status_without_any_browser() {
        use crate::notifications::Observation;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("core.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (io, _) = listener.accept().await.unwrap();
            let mut io = BufReader::new(io);
            for result in [
                json!({"generation":1}),
                json!({"seq":5,"terminals":[{"address":"t1","status":{"state":"working"}}]}),
            ] {
                let mut line = String::new();
                io.read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                io.get_mut()
                    .write_all(
                        format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            for message in [
                json!({"method":"event","params":{"seq":6,"type":"status","terminal":"t1","data":{"status":{"state":"done"},"reported":true}}}),
                // Duplicate sequence must not reach the notification observer.
                json!({"method":"event","params":{"seq":6,"type":"status","terminal":"t1","data":{"status":{"state":"error"}}}}),
                json!({"method":"missed","params":{"seq":20,"terminals":[{"address":"t1","status":{"state":"blocked"}}]}}),
            ] {
                io.get_mut()
                    .write_all(format!("{message}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let core = Core::new(path);
        let mut events = core.notification_events.subscribe();
        assert!(core.subscribe().await.is_err());
        assert!(matches!(
            events.recv().await.unwrap(),
            Observation::Snapshot(_)
        ));
        let Observation::Event { kind, record } = events.recv().await.unwrap() else {
            panic!("expected live status");
        };
        assert_eq!(kind, "status");
        assert_eq!(record["status"]["state"], "done");
        assert!(matches!(
            events.recv().await.unwrap(),
            Observation::Snapshot(_)
        ));
        assert!(events.try_recv().is_err());
        server.await.unwrap();
    }
    #[tokio::test]
    async fn persistent_rpc_handshake_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("core.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let task = tokio::spawn(async move {
            let (io, _) = listener.accept().await.unwrap();
            let mut io = BufReader::new(io);
            for result in [json!({"generation":1}), json!({"ok":true})] {
                let mut line = String::new();
                io.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                io.get_mut()
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"jsonrpc":"2.0","id":req["id"],"result":result})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let core = Core::new(path);
        assert_eq!(core.call("test", json!({})).await.unwrap()["ok"], true);
        task.await.unwrap();
    }
}
