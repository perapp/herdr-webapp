//! Connector-owned Web Push. Never log subscriptions, payloads or delivery errors.
use crate::{
    Shared,
    auth::{ApiError, error},
    relay_crypto,
};
use anyhow::{Result, ensure};
use axum::{
    Extension, Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::SigningKey;
use p256::elliptic_curve::rand_core::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex, broadcast},
    task::JoinSet,
};

const DELIVERY_QUEUE: usize = 512;
const DELIVERY_CONCURRENCY: usize = 8;
const DELIVERY_TTL: Duration = Duration::from_secs(300);
struct Delivery {
    owner_epoch: u64,
    registration: Registration,
    terminal: String,
    payload: Vec<u8>,
    created: Instant,
}
use web_push::{ContentEncoding, SubscriptionInfo, VapidSignatureBuilder, WebPushMessageBuilder};

#[derive(Clone)]
pub struct Owner(pub String);
#[derive(Clone, Debug)]
pub enum Observation {
    Snapshot(Vec<Value>),
    Removed(String),
    Event { kind: String, record: Value },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registration {
    owner: String,
    subscription: SubscriptionInfo,
    connection_id: String,
    channel: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Saved {
    registrations: Vec<Registration>,
    baseline: HashMap<String, String>,
}
#[derive(Clone, Default)]
struct OwnerPolicy {
    generation: u64,
    grants: Option<BTreeMap<String, u64>>,
}
// An empty terminal denotes a whole-page lease, inserted only by pageVisible=true.
type ForegroundLeases = HashMap<(String, String, String), (String, Instant)>;
pub struct Notifications {
    path: PathBuf,
    private_key: String,
    pub public_key: String,
    pub needs_snapshot: AtomicBool,
    saved: Mutex<Saved>,
    owners: tokio::sync::watch::Sender<OwnerPolicy>,
    seen: Mutex<ForegroundLeases>,
    // Exclusive process lock also prevents competing writers and duplicate deliveries.
    _lock: std::fs::File,
    #[cfg(test)]
    attempts: Mutex<Vec<Vec<u8>>>,
    #[cfg(test)]
    send_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}
impl Notifications {
    pub fn open(dir: &Path) -> Result<Arc<Self>> {
        relay_crypto::ensure_private_dir(dir)?;
        let lock = relay_crypto::lock_private(&dir.join("notifications.lock"))?;
        let key_path = dir.join("notifications-key.json");
        let private_key = if key_path.exists() {
            serde_json::from_slice::<String>(&relay_crypto::read_private(&key_path, 4096)?)?
        } else {
            let key = URL_SAFE_NO_PAD.encode(SigningKey::random(&mut OsRng).to_bytes());
            relay_crypto::write_private(&key_path, &serde_json::to_vec(&key)?)?;
            key
        };
        let key = SigningKey::from_slice(&relay_crypto::decode(&private_key, 32)?)?;
        let public_key =
            URL_SAFE_NO_PAD.encode(key.verifying_key().to_encoded_point(false).as_bytes());
        let path = dir.join("notifications.json");
        let saved: Saved = if path.exists() {
            serde_json::from_slice(&relay_crypto::read_private(&path, 4 << 20)?)?
        } else {
            Saved::default()
        };
        ensure!(saved.registrations.len() <= 256, "too many subscriptions");
        Ok(Arc::new(Self {
            path,
            private_key,
            public_key,
            needs_snapshot: AtomicBool::new(false),
            saved: Mutex::new(saved),
            owners: tokio::sync::watch::channel(OwnerPolicy::default()).0,
            seen: Mutex::new(HashMap::new()),
            _lock: lock,
            #[cfg(test)]
            attempts: Mutex::new(Vec::new()),
            #[cfg(test)]
            send_gate: Mutex::new(None),
        }))
    }
    /// Fail closed immediately (including queued/in-flight sends), then persist.
    /// Caller must serialize this with its authorization rotation/enrollment.
    pub async fn keep_owner(&self, owner: &str) -> Result<()> {
        self.retain_owners(&[owner.to_owned()]).await
    }
    /// Reconcile with the durable authorization store BEFORE starting observe().
    /// Caller must propagate errors and not start delivery on reconciliation failure.
    /// Runtime callers serialize this with their auth/pairing rotation gate.
    /// Surviving grants keep their epochs so their in-flight delivery continues;
    /// newly added grants never resurrect work queued before their revocation.
    pub async fn retain_owners(&self, owners: &[String]) -> Result<()> {
        let owners: BTreeSet<&str> = owners.iter().map(String::as_str).collect();
        let mut saved = self.saved.lock().await;
        self.owners.send_modify(|policy| {
            policy.generation += 1;
            let grants = owners
                .iter()
                .map(|owner| {
                    let epoch = policy.grants.as_ref().map_or(0, |grants| {
                        grants.get(*owner).copied().unwrap_or(policy.generation)
                    });
                    ((*owner).to_owned(), epoch)
                })
                .collect();
            policy.grants = Some(grants);
        });
        let mut unique = BTreeSet::new();
        saved.registrations.retain(|r| {
            owners.contains(r.owner.as_str())
                && unique.insert((r.owner.clone(), r.subscription.endpoint.clone()))
        });
        self.seen
            .lock()
            .await
            .retain(|(owner, endpoint, _), (_, at)| {
                owners.contains(owner.as_str())
                    && at.elapsed() < Duration::from_secs(30)
                    && unique.contains(&(owner.clone(), endpoint.clone()))
            });
        // Fail closed in memory even when persistence fails. Startup must retry
        // reconciliation against durable Auth/Pairings before any observation.
        self.save(&saved)
    }
    pub async fn authorize_owner(&self, owner: &str) {
        let _saved = self.saved.lock().await;
        self.owners.send_modify(|policy| {
            if let Some(grants) = &mut policy.grants {
                grants.entry(owner.to_owned()).or_insert(policy.generation);
            }
        });
    }
    fn owner_allowed(&self, owner: &str) -> bool {
        self.owner_epoch(owner).is_some()
    }
    fn owner_epoch(&self, owner: &str) -> Option<u64> {
        self.owners
            .borrow()
            .grants
            .as_ref()
            .map_or(Some(0), |grants| grants.get(owner).copied())
    }
    fn save(&self, saved: &Saved) -> Result<()> {
        let bytes = serde_json::to_vec(saved)?;
        ensure!(bytes.len() <= 4 << 20, "notification state exceeds limit");
        relay_crypto::write_private(&self.path, &bytes)
    }
    pub async fn observe(self: Arc<Self>, mut events: broadcast::Receiver<Observation>) {
        // No subscription snapshots or backlog may produce notifications.
        let mut ready = false;
        let mut queue: VecDeque<Delivery> = VecDeque::new();
        // JoinSet aborts outstanding network operations if this observer is cancelled.
        let mut deliveries = JoinSet::new();
        let mut closed = false;
        loop {
            while deliveries.len() < DELIVERY_CONCURRENCY {
                let Some(delivery) = queue.pop_front() else {
                    break;
                };
                if delivery.created.elapsed() >= DELIVERY_TTL {
                    continue;
                }
                let service = self.clone();
                deliveries.spawn(async move { service.deliver(delivery).await });
            }
            if closed && queue.is_empty() && deliveries.is_empty() {
                break;
            }
            let received = tokio::select! {
                result = events.recv(), if !closed => result,
                _ = deliveries.join_next(), if !deliveries.is_empty() => continue,
            };
            let observation = match received {
                Ok(v) => v,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    ready = false;
                    self.request_snapshot(&mut events);
                    continue;
                }
                Err(_) => {
                    closed = true;
                    continue;
                }
            };
            match observation {
                Observation::Snapshot(records) => {
                    if records.len() > 4096 {
                        ready = false;
                        self.request_snapshot(&mut events);
                        continue;
                    }
                    let mut saved = self.saved.lock().await;
                    saved.baseline.clear();
                    for record in records {
                        if let Some(address) = record["address"].as_str().filter(|s| s.len() <= 512)
                        {
                            saved.baseline.insert(address.into(), word(&record));
                        }
                    }
                    ready = self.save(&saved).is_ok();
                    if !ready {
                        self.request_snapshot(&mut events);
                    }
                }
                Observation::Removed(address) if ready => {
                    let mut saved = self.saved.lock().await;
                    if saved.baseline.remove(&address).is_some() && self.save(&saved).is_err() {
                        ready = false;
                        self.request_snapshot(&mut events);
                    }
                }
                Observation::Event { kind, record } if ready => {
                    let Some(address) = record["address"].as_str().filter(|s| s.len() <= 512)
                    else {
                        continue;
                    };
                    let next = word(&record);
                    let registrations = {
                        let mut saved = self.saved.lock().await;
                        if saved.baseline.get(address) == Some(&next) {
                            continue;
                        }
                        saved.baseline.insert(address.into(), next.clone());
                        if saved.baseline.len() > 4096 {
                            ready = false;
                            self.request_snapshot(&mut events);
                            continue;
                        }
                        // Commit consumption BEFORE network IO: ambiguous failures are never retried.
                        if self.save(&saved).is_err() {
                            ready = false;
                            self.request_snapshot(&mut events);
                            continue;
                        }
                        if kind != "status"
                            || !matches!(next.as_str(), "done" | "blocked" | "error")
                        {
                            continue;
                        }
                        saved
                            .registrations
                            .iter()
                            .filter_map(|registration| {
                                self.owner_epoch(&registration.owner)
                                    .map(|epoch| (registration.clone(), epoch))
                            })
                            .collect::<Vec<_>>()
                    };
                    let event_id = uuid::Uuid::new_v4().to_string();
                    for (registration, owner_epoch) in registrations {
                        if self.foreground(&registration, address).await {
                            continue;
                        }
                        let payload = json!({"version":1,"eventId":event_id,"kind":next,"connectionId":registration.connection_id,"channel":registration.channel,"terminal":address});
                        // Consumption is already committed: overflow is dropped, never retried.
                        if queue.len() < DELIVERY_QUEUE {
                            queue.push_back(Delivery {
                                owner_epoch,
                                registration,
                                terminal: address.to_owned(),
                                payload: serde_json::to_vec(&payload).unwrap(),
                                created: Instant::now(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }
    fn request_snapshot(&self, events: &mut broadcast::Receiver<Observation>) {
        // Discard all facts buffered before requesting a new ordered snapshot.
        *events = events.resubscribe();
        self.needs_snapshot.store(true, Ordering::Release);
    }
    async fn foreground(&self, registration: &Registration, address: &str) -> bool {
        self.seen
            .lock()
            .await
            .iter()
            .any(|((owner, endpoint, _), (terminal, at))| {
                owner == &registration.owner
                    && endpoint == &registration.subscription.endpoint
                    && (terminal.is_empty() || terminal == address)
                    && at.elapsed() < Duration::from_secs(30)
            })
    }
    async fn deliver(&self, delivery: Delivery) {
        let Some(remaining) = DELIVERY_TTL.checked_sub(delivery.created.elapsed()) else {
            return;
        };
        // Visibility may have changed while this delivery waited in the queue.
        if self
            .foreground(&delivery.registration, &delivery.terminal)
            .await
        {
            return;
        }
        // Opting out while queued must not send a stale notification either.
        if !self.saved.lock().await.registrations.iter().any(|r| {
            r.owner == delivery.registration.owner
                && r.subscription.endpoint == delivery.registration.subscription.endpoint
        }) {
            return;
        }
        let mut changes = self.owners.subscribe();
        let sending = tokio::time::timeout(
            remaining,
            self.send(&delivery.registration.subscription, &delivery.payload),
        );
        tokio::pin!(sending);
        let result = loop {
            if self.owner_epoch(&delivery.registration.owner) != Some(delivery.owner_epoch) {
                return;
            }
            tokio::select! {
                biased;
                changed = changes.changed() => { if changed.is_err() { return; } },
                result = &mut sending => break result,
            }
        };
        if let Ok(Ok(404 | 410)) = result {
            let mut saved = self.saved.lock().await;
            saved
                .registrations
                .retain(|r| r.subscription.endpoint != delivery.registration.subscription.endpoint);
            // A failed cleanup does not replay this consumed delivery.
            if self.save(&saved).is_err() {
                self.needs_snapshot.store(true, Ordering::Release);
            }
        }
    }
    #[cfg(test)]
    async fn send(&self, _subscription: &SubscriptionInfo, payload: &[u8]) -> Result<u16> {
        let gate = self.send_gate.lock().await.clone();
        let _permit = if let Some(gate) = gate {
            Some(gate.acquire_owned().await.unwrap())
        } else {
            None
        };
        self.attempts.lock().await.push(payload.to_vec());
        Ok(201)
    }
    #[cfg(not(test))]
    async fn send(&self, subscription: &SubscriptionInfo, payload: &[u8]) -> Result<u16> {
        let url = endpoint(&subscription.endpoint)?;
        let host = url.host_str().unwrap();
        let addresses: Vec<_> =
            tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host((host, 443)))
                .await??
                .collect();
        ensure!(
            !addresses.is_empty() && addresses.iter().all(|a| public_ip(a.ip())),
            "unsafe push DNS"
        );
        // Pin validated addresses, disable proxies and redirects: no DNS rebinding/redirect SSRF.
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .resolve_to_addrs(host, &addresses)
            .build()?;
        let mut signature = VapidSignatureBuilder::from_base64(&self.private_key, subscription)?;
        signature.add_claim("sub", "https://herdr-webapp.perapp.dev/");
        let mut builder = WebPushMessageBuilder::new(subscription);
        builder.set_payload(ContentEncoding::Aes128Gcm, payload);
        builder.set_vapid_signature(signature.build()?);
        builder.set_ttl(300);
        let request = web_push::request_builder::build_request::<Vec<u8>>(builder.build()?);
        let (parts, body) = request.into_parts();
        let mut request = client.post(url).body(body);
        for (name, value) in &parts.headers {
            request = request.header(name.as_str(), value.as_bytes());
        }
        let response = request.send().await?;
        Ok(response.status().as_u16())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    async fn wait_until(mut condition: impl AsyncFnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !condition().await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    fn record(address: &str, state: &str) -> Value {
        json!({"address":address,"status":{"state":state}})
    }
    async fn registered_service(dir: &Path) -> Arc<Notifications> {
        let service = Notifications::open(&dir.join("push")).unwrap();
        service.saved.lock().await.registrations.push(Registration {
            owner: "alice".into(),
            subscription: SubscriptionInfo::new(
                "https://fcm.googleapis.com/test",
                "unused",
                "unused",
            ),
            connection_id: "computer".into(),
            channel: "stable".into(),
        });
        service
    }
    #[tokio::test]
    async fn lag_recovery_discards_buffered_facts_and_does_not_replay_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let service = registered_service(dir.path()).await;
        let (tx, rx) = broadcast::channel(2);
        // Force lag before the observer starts; even the buffered snapshot is stale.
        tx.send(Observation::Snapshot(vec![record("t1", "working")]))
            .unwrap();
        for _ in 0..4 {
            tx.send(Observation::Event {
                kind: "status".into(),
                record: record("t1", "done"),
            })
            .unwrap();
        }
        let observer = tokio::spawn(service.clone().observe(rx));
        wait_until(async || service.needs_snapshot.load(Ordering::Acquire)).await;
        assert!(service.saved.lock().await.baseline.is_empty());
        // Core's ordered publisher acknowledges the request then seeds current state.
        service.needs_snapshot.swap(false, Ordering::AcqRel);
        tx.send(Observation::Snapshot(vec![record("t1", "done")]))
            .unwrap();
        wait_until(async || {
            service
                .saved
                .lock()
                .await
                .baseline
                .get("t1")
                .is_some_and(|v| v == "done")
        })
        .await;
        tx.send(Observation::Event {
            kind: "status".into(),
            record: record("t1", "done"),
        })
        .unwrap();
        tx.send(Observation::Event {
            kind: "status".into(),
            record: record("t1", "blocked"),
        })
        .unwrap();
        drop(tx);
        observer.await.unwrap();
        assert_eq!(service.attempts.lock().await.len(), 1);
    }
    #[tokio::test]
    async fn capacity_and_disk_failures_request_snapshot_and_recover_without_replay() {
        for disk_failure in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let service = registered_service(dir.path()).await;
            if disk_failure {
                // Atomic replacement cannot replace a directory with the state file.
                std::fs::create_dir(&service.path).unwrap();
            }
            let (tx, rx) = broadcast::channel(16);
            let observer = tokio::spawn(service.clone().observe(rx));
            let records = if disk_failure {
                vec![record("t1", "working")]
            } else {
                (0..4097)
                    .map(|i| record(&format!("t{i}"), "working"))
                    .collect()
            };
            tx.send(Observation::Snapshot(records)).unwrap();
            wait_until(async || service.needs_snapshot.load(Ordering::Acquire)).await;
            assert!(service.attempts.lock().await.is_empty());
            if disk_failure {
                std::fs::remove_dir(&service.path).unwrap();
            }
            service.needs_snapshot.swap(false, Ordering::AcqRel);
            tx.send(Observation::Event {
                kind: "status".into(),
                record: record("t1", "error"),
            })
            .unwrap();
            tx.send(Observation::Snapshot(vec![record("t1", "done")]))
                .unwrap();
            wait_until(async || {
                service
                    .saved
                    .lock()
                    .await
                    .baseline
                    .get("t1")
                    .is_some_and(|v| v == "done")
            })
            .await;
            tx.send(Observation::Event {
                kind: "status".into(),
                record: record("t1", "blocked"),
            })
            .unwrap();
            drop(tx);
            observer.await.unwrap();
            assert_eq!(service.attempts.lock().await.len(), 1);
            assert!(!service.needs_snapshot.load(Ordering::Acquire));
        }
    }
    #[tokio::test]
    async fn removal_churn_prunes_baseline_beyond_lifetime_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let service = registered_service(dir.path()).await;
        let (tx, rx) = broadcast::channel(16);
        let observer = tokio::spawn(service.clone().observe(rx));
        tx.send(Observation::Snapshot(vec![])).unwrap();
        // More than 4096 lifetime addresses but never more than one live record.
        for index in 0..4100 {
            let address = format!("t{index}");
            tx.send(Observation::Event {
                kind: "created".into(),
                record: record(&address, "working"),
            })
            .unwrap();
            wait_until(async || service.saved.lock().await.baseline.contains_key(&address)).await;
            tx.send(Observation::Removed(address.clone())).unwrap();
            wait_until(async || !service.saved.lock().await.baseline.contains_key(&address)).await;
        }
        tx.send(Observation::Event {
            kind: "status".into(),
            record: record("final", "done"),
        })
        .unwrap();
        drop(tx);
        observer.await.unwrap();
        assert!(!service.needs_snapshot.load(Ordering::Acquire));
        assert_eq!(service.saved.lock().await.baseline.len(), 1);
        assert_eq!(service.attempts.lock().await.len(), 1);
    }
    #[tokio::test]
    async fn slow_delivery_is_bounded_and_does_not_block_consumption_and_close_drains() {
        let dir = tempfile::tempdir().unwrap();
        let service = registered_service(dir.path()).await;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *service.send_gate.lock().await = Some(gate.clone());
        let (tx, rx) = broadcast::channel(2048);
        let observer = tokio::spawn(service.clone().observe(rx));
        tx.send(Observation::Snapshot(vec![])).unwrap();
        for index in 0..600 {
            tx.send(Observation::Event {
                kind: "status".into(),
                record: record(&format!("t{index}"), "done"),
            })
            .unwrap();
        }
        wait_until(async || service.saved.lock().await.baseline.len() == 600).await;
        assert!(service.attempts.lock().await.is_empty());
        drop(tx);
        gate.add_permits(DELIVERY_CONCURRENCY);
        observer.await.unwrap();
        assert_eq!(
            service.attempts.lock().await.len(),
            DELIVERY_QUEUE + DELIVERY_CONCURRENCY
        );
        assert!(!service.needs_snapshot.load(Ordering::Acquire));
    }
    #[tokio::test]
    async fn cancellation_aborts_inflight_delivery_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let service = registered_service(dir.path()).await;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *service.send_gate.lock().await = Some(gate.clone());
        let (tx, rx) = broadcast::channel(16);
        let observer = tokio::spawn(service.clone().observe(rx));
        tx.send(Observation::Snapshot(vec![])).unwrap();
        tx.send(Observation::Event {
            kind: "status".into(),
            record: record("t1", "done"),
        })
        .unwrap();
        // The network task owns a cloned semaphore while it is suspended.
        wait_until(async || Arc::strong_count(&gate) > 2).await;
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        wait_until(async || Arc::strong_count(&gate) == 2).await;
        gate.add_permits(1);
        assert!(service.attempts.lock().await.is_empty());
        drop(tx);
    }
    #[tokio::test]
    async fn startup_reconciliation_repairs_stale_push_file_after_rotation_cleanup_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let service = registered_service(dir.path()).await;
        let revoked = {
            let mut saved = service.saved.lock().await;
            let keeper = saved.registrations[0].clone();
            let mut revoked = keeper.clone();
            revoked.owner = "bob".into();
            revoked.connection_id = "revoked".into();
            revoked.subscription.endpoint = "https://fcm.googleapis.com/bob".into();
            let mut legacy = keeper.clone();
            legacy.owner = "local".into();
            saved
                .registrations
                .extend([keeper, revoked.clone(), legacy]);
            service.save(&saved).unwrap();
            revoked
        };
        std::fs::set_permissions(&service.path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(service.keep_owner("alice").await.is_err());
        assert!(!service.owner_allowed("bob"));
        std::fs::set_permissions(&service.path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let stale: Saved =
            serde_json::from_slice(&relay_crypto::read_private(&service.path, 1 << 20).unwrap())
                .unwrap();
        assert!(stale.registrations.iter().any(|r| r.owner == "bob"));
        drop(service);
        let service = Notifications::open(&dir.path().join("push")).unwrap();
        // Only the durable Auth/Pairings allowlist may restore authorization.
        service.seen.lock().await.insert(
            (
                "bob".into(),
                revoked.subscription.endpoint.clone(),
                "tab".into(),
            ),
            ("t1".into(), Instant::now()),
        );
        service.retain_owners(&["alice".to_owned()]).await.unwrap();
        assert!(!service.owner_allowed("bob"));
        assert!(!service.owner_allowed("local"));
        assert!(service.seen.lock().await.is_empty());
        let repaired: Saved =
            serde_json::from_slice(&relay_crypto::read_private(&service.path, 1 << 20).unwrap())
                .unwrap();
        assert_eq!(repaired.registrations.len(), 1);
        assert_eq!(repaired.registrations[0].owner, "alice");
        service
            .deliver(Delivery {
                owner_epoch: 0,
                registration: revoked,
                terminal: "t1".into(),
                payload: vec![],
                created: Instant::now(),
            })
            .await;
        assert!(service.attempts.lock().await.is_empty());
        let (tx, rx) = broadcast::channel(8);
        tx.send(Observation::Snapshot(vec![])).unwrap();
        tx.send(Observation::Event {
            kind: "status".into(),
            record: record("t1", "done"),
        })
        .unwrap();
        drop(tx);
        service.clone().observe(rx).await;
        assert_eq!(service.attempts.lock().await.len(), 1);
    }
    #[tokio::test]
    async fn retaining_owner_preserves_inflight_delivery_and_existing_grant_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let service = registered_service(dir.path()).await;
        service
            .retain_owners(&["alice".into(), "bob".into()])
            .await
            .unwrap();
        let epoch = service.owner_epoch("alice").unwrap();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *service.send_gate.lock().await = Some(gate.clone());
        let registration = service.saved.lock().await.registrations[0].clone();
        let delivery_service = service.clone();
        let task = tokio::spawn(async move {
            delivery_service
                .deliver(Delivery {
                    owner_epoch: epoch,
                    registration,
                    terminal: "t1".into(),
                    payload: vec![],
                    created: Instant::now(),
                })
                .await
        });
        wait_until(async || Arc::strong_count(&gate) > 2).await;
        service.keep_owner("alice").await.unwrap();
        assert_eq!(service.owner_epoch("alice"), Some(epoch));
        assert!(!service.owner_allowed("bob"));
        gate.add_permits(1);
        task.await.unwrap();
        assert_eq!(service.attempts.lock().await.len(), 1);
    }
    #[tokio::test]
    async fn rotation_revokes_queued_and_inflight_push_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let service = registered_service(dir.path()).await;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *service.send_gate.lock().await = Some(gate.clone());
        let (tx, rx) = broadcast::channel(64);
        let observer = tokio::spawn(service.clone().observe(rx));
        tx.send(Observation::Snapshot(vec![])).unwrap();
        for n in 0..20 {
            tx.send(Observation::Event {
                kind: "status".into(),
                record: record(&format!("t{n}"), "done"),
            })
            .unwrap();
        }
        wait_until(async || {
            service.saved.lock().await.baseline.len() == 20 && Arc::strong_count(&gate) > 2
        })
        .await;
        service.keep_owner("bob").await.unwrap();
        assert!(!service.owner_allowed("alice"));
        assert!(service.owner_allowed("bob"));
        drop(tx);
        observer.await.unwrap();
        gate.add_permits(20);
        assert!(service.attempts.lock().await.is_empty());
        assert!(service.saved.lock().await.registrations.is_empty());
        drop(service);
        let reopened = Notifications::open(&dir.path().join("push")).unwrap();
        assert!(reopened.saved.lock().await.registrations.is_empty());
    }
    #[tokio::test]
    async fn expired_delivery_is_dropped_without_send() {
        let dir = tempfile::tempdir().unwrap();
        let service = registered_service(dir.path()).await;
        service
            .deliver(Delivery {
                owner_epoch: 0,
                registration: service.saved.lock().await.registrations[0].clone(),
                terminal: "t1".into(),
                payload: vec![],
                created: Instant::now() - DELIVERY_TTL - Duration::from_secs(1),
            })
            .await;
        assert!(service.attempts.lock().await.is_empty());
    }
    #[test]
    fn strict_provider_and_public_address_policy() {
        for endpoint_url in [
            "https://fcm.googleapis.com/fcm/send/test",
            "https://updates.push.services.mozilla.com/wpush/v2/test",
            "https://web.push.apple.com/Qtest",
        ] {
            assert!(endpoint(endpoint_url).is_ok());
        }
        for endpoint_url in [
            "http://fcm.googleapis.com/test",
            "https://fcm.googleapis.com.evil.test/test",
            "https://evil.fcm.googleapis.com/test",
            "https://user@fcm.googleapis.com/test",
            "https://fcm.googleapis.com:444/test",
            "https://fcm.googleapis.com/test#fragment",
            "https://127.0.0.1/test",
            "https://example.com/test",
        ] {
            assert!(endpoint(endpoint_url).is_err());
        }
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "0.0.0.0",
            "::1",
            "::ffff:127.0.0.1",
            "fd00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
    }
    #[tokio::test]
    async fn private_persistent_key_and_exclusive_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("push");
        let service = Notifications::open(&path).unwrap();
        let public_key = service.public_key.clone();
        assert!(Notifications::open(&path).is_err());
        let subscription = SubscriptionInfo::new(
            "https://fcm.googleapis.com/fcm/send/test".to_owned(),
            public_key.clone(),
            URL_SAFE_NO_PAD.encode([7u8; 16]),
        );
        let signature = VapidSignatureBuilder::from_base64(&service.private_key, &subscription)
            .unwrap()
            .build()
            .unwrap();
        let mut builder = WebPushMessageBuilder::new(&subscription);
        builder.set_vapid_signature(signature);
        builder.set_payload(ContentEncoding::Aes128Gcm, b"private-test-payload");
        let request = web_push::request_builder::build_request::<Vec<u8>>(builder.build().unwrap());
        assert_eq!(request.headers()["content-encoding"], "aes128gcm");
        assert!(request.headers().contains_key("authorization"));
        assert!(
            !request
                .body()
                .windows(20)
                .any(|w| w == b"private-test-payload")
        );
        drop(service);
        assert_eq!(Notifications::open(&path).unwrap().public_key, public_key);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path.join("notifications-key.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    async fn call(
        app: Shared,
        owner: &str,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", "Bearer test-token")
            .header("origin", "http://test.local")
            .header("host", "test.local")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        request.extensions_mut().insert(Owner(owner.into()));
        let response = crate::router(app).oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }
    async fn local_call(app: Shared, cookie: &str, path: &str, body: Value) -> StatusCode {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("cookie", cookie)
            .header("origin", "http://test.local")
            .header("host", "test.local")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        crate::router(app).oneshot(request).await.unwrap().status()
    }
    fn cookie_headers(cookie: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("cookie", cookie.parse().unwrap());
        headers.insert("origin", "http://test.local".parse().unwrap());
        headers.insert("host", "test.local".parse().unwrap());
        headers
    }
    #[tokio::test]
    async fn lan_rotation_keeps_profile_push_allows_new_login_and_denies_waiting_stale_request() {
        let dir = tempfile::tempdir().unwrap();
        let service = Notifications::open(&dir.path().join("push")).unwrap();
        let core = crate::core::Core::new("/nonexistent".into());
        let _ = core.notifications.set(service.clone());
        let app = Arc::new(crate::App {
            core,
            auth: crate::auth::Auth::new("test-token".into(), Some("http://test.local".into())),
            updates: crate::update::Updater::development(),
        });
        let keeper = app.auth.login("test-token").unwrap();
        let other = app.auth.login("test-token").unwrap();
        let keeper_owner = app
            .auth
            .notification_owner(&cookie_headers(&keeper))
            .unwrap();
        let other_owner = app
            .auth
            .notification_owner(&cookie_headers(&other))
            .unwrap();
        assert_ne!(keeper_owner, other_owner);
        let registration = |device: &str| json!({"subscription":{"endpoint":format!("https://fcm.googleapis.com/fcm/send/{device}"),"keys":{"p256dh":service.public_key,"auth":URL_SAFE_NO_PAD.encode([7u8;16])}},"connectionId":"computer","channel":"stable"});
        assert_eq!(
            local_call(
                app.clone(),
                &keeper,
                "/api/notifications",
                registration("keeper")
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            local_call(
                app.clone(),
                &other,
                "/api/notifications",
                registration("other")
            )
            .await,
            StatusCode::OK
        );
        {
            let saved = service.saved.lock().await;
            assert_eq!(saved.registrations.len(), 2);
            assert!(saved.registrations.iter().any(|r| r.owner == keeper_owner));
            assert!(saved.registrations.iter().any(|r| r.owner == other_owner));
        }
        // Queue an old request behind the same gate held by rotation. It must
        // recheck authorization after waking, never regrant its revoked owner.
        let write_guard = app.auth.authorization_write().await;
        let stale_app = app.clone();
        let stale_cookie = other.clone();
        let stale_body = registration("stale");
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let stale_request = tokio::spawn(async move {
            entered.send(()).unwrap();
            local_call(stale_app, &stale_cookie, "/api/notifications", stale_body).await
        });
        waiting.await.unwrap();
        let retained = app.auth.rotate(&cookie_headers(&keeper)).unwrap();
        assert_eq!(retained, keeper_owner);
        service.keep_owner(&retained).await.unwrap();
        drop(write_guard);
        assert_eq!(stale_request.await.unwrap(), StatusCode::UNAUTHORIZED);
        assert!(!service.owner_allowed(&other_owner));
        assert_eq!(
            local_call(
                app.clone(),
                &keeper,
                "/api/notifications",
                registration("keeper")
            )
            .await,
            StatusCode::OK
        );
        let new_cookie = app
            .auth
            .login(&app.auth.current_token_snapshot().unwrap().token)
            .unwrap();
        let new_owner = app
            .auth
            .notification_owner(&cookie_headers(&new_cookie))
            .unwrap();
        assert_ne!(new_owner, keeper_owner);
        assert_eq!(
            local_call(
                app.clone(),
                &new_cookie,
                "/api/notifications",
                registration("new")
            )
            .await,
            StatusCode::OK
        );
        assert!(service.owner_allowed(&new_owner));
        let saved = service.saved.lock().await;
        assert_eq!(saved.registrations.len(), 2);
        assert!(
            saved
                .registrations
                .iter()
                .all(|r| r.owner == keeper_owner || r.owner == new_owner)
        );
        let persisted: Saved =
            serde_json::from_slice(&relay_crypto::read_private(&service.path, 1 << 20).unwrap())
                .unwrap();
        assert!(
            persisted
                .registrations
                .iter()
                .all(|r| r.owner != other_owner)
        );
    }
    #[tokio::test]
    async fn authenticated_device_isolation_and_per_client_foreground_leases() {
        let dir = tempfile::tempdir().unwrap();
        let service = Notifications::open(&dir.path().join("push")).unwrap();
        let core = crate::core::Core::new("/nonexistent".into());
        let _ = core.notifications.set(service.clone());
        let app = Arc::new(crate::App {
            core,
            auth: crate::auth::Auth::new("test-token".into(), Some("http://test.local".into())),
            updates: crate::update::Updater::development(),
        });
        let endpoint_url = "https://fcm.googleapis.com/fcm/send/test";
        let register = json!({"subscription":{"endpoint":endpoint_url,"keys":{"p256dh":service.public_key,"auth":URL_SAFE_NO_PAD.encode([7u8;16])}},"connectionId":"computer","channel":"stable"});
        assert_eq!(
            call(app.clone(), "alice", "POST", "/api/notifications", register)
                .await
                .0,
            StatusCode::OK
        );
        for client in ["tabA", "tabB"] {
            assert_eq!(
                call(
                    app.clone(),
                    "alice",
                    "POST",
                    "/api/notifications/seen",
                    json!({"endpoint":endpoint_url,"terminal":"t1","clientId":client})
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        assert_eq!(service.seen.lock().await.len(), 2);
        assert_eq!(
            call(
                app.clone(),
                "alice",
                "POST",
                "/api/notifications/seen",
                json!({"endpoint":endpoint_url,"terminal":"","clientId":"tabA"})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(service.seen.lock().await.len(), 1);
        assert_eq!(
            call(
                app.clone(),
                "bob",
                "POST",
                "/api/notifications/delete",
                json!({"endpoint":endpoint_url})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(service.saved.lock().await.registrations.len(), 1);
        assert_eq!(
            call(
                app.clone(),
                "bob",
                "POST",
                "/api/notifications/seen",
                json!({"endpoint":endpoint_url,"terminal":"t1","clientId":"tabA"})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                app.clone(),
                "alice",
                "POST",
                "/api/notifications/delete",
                json!({"endpoint":endpoint_url})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(service.saved.lock().await.registrations.is_empty());
        let response = crate::router(app)
            .oneshot(
                Request::builder()
                    .uri("/api/notifications")
                    .header("origin", "http://test.local")
                    .header("host", "test.local")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    #[tokio::test]
    async fn visible_page_leases_are_scoped_expire_and_preserve_legacy_requests() {
        let dir = tempfile::tempdir().unwrap();
        let service = registered_service(dir.path()).await;
        let registration = service.saved.lock().await.registrations[0].clone();
        let endpoint_url = registration.subscription.endpoint.clone();
        let mut other_device = registration.clone();
        other_device.subscription.endpoint = "https://fcm.googleapis.com/other".into();
        let mut other_owner = registration.clone();
        other_owner.owner = "bob".into();
        service
            .saved
            .lock()
            .await
            .registrations
            .extend([other_device.clone(), other_owner.clone()]);
        let core = crate::core::Core::new("/nonexistent".into());
        let _ = core.notifications.set(service.clone());
        let app = Arc::new(crate::App {
            core,
            auth: crate::auth::Auth::new("test-token".into(), Some("http://test.local".into())),
            updates: crate::update::Updater::development(),
        });
        for client in ["tabA", "tabB"] {
            assert_eq!(
                call(app.clone(), "alice", "POST", "/api/notifications/seen",
                    json!({"endpoint":endpoint_url,"terminal":"t1","clientId":client,"pageVisible":true}))
                    .await.0,
                StatusCode::OK
            );
        }
        assert!(service.foreground(&registration, "t2").await);
        assert!(!service.foreground(&other_device, "t2").await);
        assert!(!service.foreground(&other_owner, "t2").await);
        // A hidden tab releases only its own lease, not another visible tab's.
        assert_eq!(
            call(
                app.clone(),
                "alice",
                "POST",
                "/api/notifications/seen",
                json!({"endpoint":endpoint_url,"terminal":"","clientId":"tabA","pageVisible":false})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(service.foreground(&registration, "t2").await);
        assert_eq!(
            call(
                app.clone(),
                "alice",
                "POST",
                "/api/notifications/seen",
                json!({"endpoint":endpoint_url,"terminal":"","clientId":"tabB","pageVisible":false})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(!service.foreground(&registration, "t2").await);
        // Empty terminal still acquires a whole-page lease when explicitly visible.
        assert_eq!(
            call(
                app.clone(),
                "alice",
                "POST",
                "/api/notifications/seen",
                json!({"endpoint":endpoint_url,"terminal":"","clientId":"tabA","pageVisible":true})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(service.foreground(&registration, "t2").await);
        for (_, at) in service.seen.lock().await.values_mut() {
            *at = Instant::now() - Duration::from_secs(31);
        }
        assert!(!service.foreground(&registration, "t1").await);
        assert!(!service.foreground(&registration, "t2").await);
        // Missing pageVisible defaults to false: old clients suppress one terminal.
        assert_eq!(
            call(
                app.clone(),
                "alice",
                "POST",
                "/api/notifications/seen",
                json!({"endpoint":endpoint_url,"terminal":"t1","clientId":"tabA"})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(service.foreground(&registration, "t1").await);
        assert!(!service.foreground(&registration, "t2").await);
        assert_eq!(
            call(
                app.clone(),
                "alice",
                "POST",
                "/api/notifications/seen",
                json!({"endpoint":endpoint_url,"terminal":"","clientId":"tabA"})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(service.seen.lock().await.is_empty());
    }
    #[tokio::test]
    async fn visible_page_suppresses_other_terminal_at_queue_and_delivery_time() {
        let dir = tempfile::tempdir().unwrap();
        let service = registered_service(dir.path()).await;
        let registration = service.saved.lock().await.registrations[0].clone();
        service.seen.lock().await.insert(
            (
                registration.owner.clone(),
                registration.subscription.endpoint.clone(),
                "tab".into(),
            ),
            (String::new(), Instant::now()),
        );
        let (tx, rx) = broadcast::channel(16);
        tx.send(Observation::Snapshot(vec![record("t2", "working")]))
            .unwrap();
        tx.send(Observation::Event {
            kind: "status".into(),
            record: record("t2", "done"),
        })
        .unwrap();
        drop(tx);
        service.clone().observe(rx).await;
        assert!(service.attempts.lock().await.is_empty());
        assert_eq!(service.saved.lock().await.baseline["t2"], "done");
        // A delivery already queued before the visible heartbeat is also dropped.
        service
            .deliver(Delivery {
                owner_epoch: 0,
                registration: registration.clone(),
                terminal: "t3".into(),
                payload: vec![],
                created: Instant::now(),
            })
            .await;
        assert!(service.attempts.lock().await.is_empty());
        service.seen.lock().await.clear();
        service
            .deliver(Delivery {
                owner_epoch: 0,
                registration,
                terminal: "t3".into(),
                payload: vec![],
                created: Instant::now(),
            })
            .await;
        assert_eq!(service.attempts.lock().await.len(), 1);
    }
    #[tokio::test]
    async fn foreground_suppresses_only_matching_device_and_pane() {
        let dir = tempfile::tempdir().unwrap();
        let service = Notifications::open(&dir.path().join("push")).unwrap();
        for owner in ["alice", "bob"] {
            service.saved.lock().await.registrations.push(Registration {
                owner: owner.into(),
                subscription: SubscriptionInfo::new(
                    format!("https://fcm.googleapis.com/{owner}"),
                    "unused".into(),
                    "unused".into(),
                ),
                connection_id: owner.into(),
                channel: "stable".into(),
            });
        }
        service.seen.lock().await.insert(
            (
                "alice".into(),
                "https://fcm.googleapis.com/alice".into(),
                "tabA".into(),
            ),
            ("t1".into(), Instant::now()),
        );
        let (tx, rx) = broadcast::channel(32);
        let observer = tokio::spawn(service.clone().observe(rx));
        let record =
            |terminal: &str, state: &str| json!({"address":terminal,"status":{"state":state}});
        tx.send(Observation::Snapshot(vec![
            record("t1", "working"),
            record("t2", "working"),
        ]))
        .unwrap();
        tx.send(Observation::Event {
            kind: "status".into(),
            record: record("t1", "done"),
        })
        .unwrap();
        tx.send(Observation::Event {
            kind: "status".into(),
            record: record("t2", "error"),
        })
        .unwrap();
        drop(tx);
        observer.await.unwrap();
        let attempts = service.attempts.lock().await;
        let payloads: Vec<Value> = attempts
            .iter()
            .map(|v| serde_json::from_slice(v).unwrap())
            .collect();
        assert_eq!(payloads.len(), 3);
        assert!(
            !payloads
                .iter()
                .any(|p| p["connectionId"] == "alice" && p["terminal"] == "t1")
        );
        assert_eq!(payloads.iter().filter(|p| p["terminal"] == "t2").count(), 2);
    }
    #[tokio::test]
    async fn observer_snapshots_repeated_reports_and_restart_do_not_replay() {
        let dir = tempfile::tempdir().unwrap();
        let service = Notifications::open(&dir.path().join("push")).unwrap();
        service.saved.lock().await.registrations.push(Registration {
            owner: "alice".into(),
            subscription: SubscriptionInfo::new(
                "https://fcm.googleapis.com/test",
                "unused",
                "unused",
            ),
            connection_id: "computer".into(),
            channel: "stable".into(),
        });
        let (tx, rx) = broadcast::channel(32);
        let observer = tokio::spawn(service.clone().observe(rx));
        let record = |state: &str| json!({"address":"t1","status":{"state":state}});
        tx.send(Observation::Snapshot(vec![record("done")]))
            .unwrap();
        tx.send(Observation::Event {
            kind: "status".into(),
            record: record("done"),
        })
        .unwrap();
        tx.send(Observation::Event {
            kind: "status".into(),
            record: record("working"),
        })
        .unwrap();
        tx.send(Observation::Event {
            kind: "status".into(),
            record: record("blocked"),
        })
        .unwrap();
        tx.send(Observation::Snapshot(vec![record("error")]))
            .unwrap();
        drop(tx);
        observer.await.unwrap();
        let attempts = service.attempts.lock().await;
        assert_eq!(attempts.len(), 1);
        assert_eq!(
            serde_json::from_slice::<Value>(&attempts[0]).unwrap()["kind"],
            "blocked"
        );
        drop(attempts);
        let saved: Saved =
            serde_json::from_slice(&relay_crypto::read_private(&service.path, 1 << 20).unwrap())
                .unwrap();
        assert_eq!(saved.baseline["t1"], "error");
        drop(service);
        let reopened = Notifications::open(&dir.path().join("push")).unwrap();
        assert_eq!(reopened.saved.lock().await.baseline["t1"], "error");
    }
}

fn word(record: &Value) -> String {
    record["status"]["state"].as_str().unwrap_or("").to_owned()
}
fn public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.octets()[0] == 0
                || ip.octets()[0] >= 224
                || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
                || (ip.octets()[0] == 198 && (18..=19).contains(&ip.octets()[1])))
        }
        std::net::IpAddr::V6(ip) => {
            ip.segments()[0] & 0xe000 == 0x2000
                && !(ip.segments()[0] == 0x2001 && ip.segments()[1] == 0xdb8)
        }
    }
}
fn endpoint(value: &str) -> Result<url::Url> {
    ensure!(value.len() <= 4096, "endpoint too long");
    let url = url::Url::parse(value)?;
    let host = url.host_str().unwrap_or("");
    // Explicit providers, NOT an arbitrary HTTPS URL or arbitrary provider subdomain.
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none_or(|p| p == 443)
            && url.fragment().is_none()
            && matches!(
                host,
                "fcm.googleapis.com" | "updates.push.services.mozilla.com" | "web.push.apple.com"
            )
            && url.path() != "/",
        "unsupported push endpoint"
    );
    Ok(url)
}
pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/notifications", get(config).post(register))
        .route("/api/notifications/delete", post(delete))
        .route("/api/notifications/seen", post(seen))
}
fn authorized(app: &Shared, headers: &HeaderMap) -> Result<Arc<Notifications>, ApiError> {
    app.auth.same_origin(headers, true)?;
    if !app.auth.authorized(headers) {
        return Err(error(StatusCode::UNAUTHORIZED, "Authentication required"));
    }
    app.core.notifications.get().cloned().ok_or_else(|| {
        error(
            StatusCode::NOT_IMPLEMENTED,
            "Notifications unavailable on this backend",
        )
    })
}
async fn config(
    State(app): State<Shared>,
    extension: Option<Extension<Owner>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let _authorization = if extension.is_none() {
        Some(app.auth.authorization_read().await)
    } else {
        None
    };
    app.auth.same_origin(&headers, false)?;
    if !app.auth.authorized(&headers) {
        return Err(error(StatusCode::UNAUTHORIZED, "Authentication required"));
    }
    Ok(Json(match app.core.notifications.get() {
        Some(n) => json!({"supported":true,"publicKey":n.public_key}),
        None => json!({"supported":false}),
    }))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Register {
    subscription: SubscriptionInfo,
    connection_id: String,
    channel: String,
}
fn owner(
    extension: Option<Extension<Owner>>,
    app: &Shared,
    headers: &HeaderMap,
) -> Result<String, ApiError> {
    // Extensions are supplied only by the cryptographically verified peer adapter;
    // LAN ownership always comes from the current cookie/bearer authorization.
    match extension {
        Some(owner) => Ok(owner.0.0),
        None => app.auth.notification_owner(headers),
    }
}
async fn register(
    State(app): State<Shared>,
    extension: Option<Extension<Owner>>,
    headers: HeaderMap,
    Json(body): Json<Register>,
) -> Result<Json<Value>, ApiError> {
    let local = extension.is_none();
    // Lock before checking credentials: a waiting request may have been revoked.
    // Rotation holds the corresponding write guard through auth + push cleanup.
    let _authorization = if local {
        Some(app.auth.authorization_read().await)
    } else {
        None
    };
    let service = authorized(&app, &headers)?;
    let owner = owner(extension, &app, &headers)?;
    let result: Result<()> = async {
        endpoint(&body.subscription.endpoint)?;
        relay_crypto::decode(&body.subscription.keys.auth, 16)?;
        p256::PublicKey::from_sec1_bytes(&relay_crypto::decode(
            &body.subscription.keys.p256dh,
            65,
        )?)?;
        ensure!(
            !body.connection_id.is_empty()
                && body.connection_id.len() <= 128
                && body
                    .connection_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                && matches!(body.channel.as_str(), "stable" | "main"),
            "invalid destination"
        );
        if local {
            service.authorize_owner(&owner).await;
        }
        let mut current = service.saved.lock().await;
        let mut saved = current.clone();
        ensure!(
            service.owner_allowed(&owner),
            "browser authorization revoked"
        );
        let registration = Registration {
            owner: owner.clone(),
            subscription: body.subscription,
            connection_id: body.connection_id,
            channel: body.channel,
        };
        if let Some(old) = saved.registrations.iter_mut().find(|r| {
            r.owner == owner && r.subscription.endpoint == registration.subscription.endpoint
        }) {
            *old = registration;
        } else {
            ensure!(saved.registrations.len() < 256, "subscription limit");
            saved.registrations.push(registration);
        }
        service.save(&saved)?;
        *current = saved;
        Ok(())
    }
    .await;
    result.map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "Invalid subscription or private storage unavailable",
        )
    })?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    endpoint: String,
}
async fn delete(
    State(app): State<Shared>,
    extension: Option<Extension<Owner>>,
    headers: HeaderMap,
    Json(body): Json<Endpoint>,
) -> Result<Json<Value>, ApiError> {
    let _authorization = if extension.is_none() {
        Some(app.auth.authorization_read().await)
    } else {
        None
    };
    let service = authorized(&app, &headers)?;
    let owner = owner(extension, &app, &headers)?;
    let mut current = service.saved.lock().await;
    let mut saved = current.clone();
    saved
        .registrations
        .retain(|r| r.owner != owner || r.subscription.endpoint != body.endpoint);
    service.save(&saved).map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Private storage unavailable",
        )
    })?;
    *current = saved;
    service
        .seen
        .lock()
        .await
        .retain(|(o, e, _), _| o != &owner || e != &body.endpoint);
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Seen {
    endpoint: String,
    terminal: String,
    client_id: String,
    #[serde(default)]
    page_visible: bool,
}
async fn seen(
    State(app): State<Shared>,
    extension: Option<Extension<Owner>>,
    headers: HeaderMap,
    Json(body): Json<Seen>,
) -> Result<Json<Value>, ApiError> {
    let _authorization = if extension.is_none() {
        Some(app.auth.authorization_read().await)
    } else {
        None
    };
    let service = authorized(&app, &headers)?;
    let owner = owner(extension, &app, &headers)?;
    if body.client_id.is_empty()
        || body.client_id.len() > 128
        || !body
            .client_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        || body.terminal.len() > 512
        || !service
            .saved
            .lock()
            .await
            .registrations
            .iter()
            .any(|r| r.owner == owner && r.subscription.endpoint == body.endpoint)
    {
        return Err(error(StatusCode::BAD_REQUEST, "Unknown subscription"));
    }
    let mut seen = service.seen.lock().await;
    seen.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(30));
    let key = (owner, body.endpoint, body.client_id);
    if !body.page_visible && body.terminal.is_empty() {
        seen.remove(&key);
    } else {
        if seen.len() >= 2048 && !seen.contains_key(&key) {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many foreground clients",
            ));
        }
        let terminal = if body.page_visible {
            String::new()
        } else {
            body.terminal
        };
        seen.insert(key, (terminal, Instant::now()));
    }
    Ok(Json(json!({"ok":true})))
}
