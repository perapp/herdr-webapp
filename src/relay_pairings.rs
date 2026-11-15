//! Serialized, durable connector capability and browser-public-key authorization.
use crate::relay_crypto::{self, random_token};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

// Largest interoperable JS integer. Keep the v1 numeric expiry wire contract for
// old clients, but never apply time-based expiry to a migrated capability.
const PERMANENT_EXPIRY: u64 = 9_007_199_254_740_991;
const RESERVATION_TIMEOUT: Duration = Duration::from_secs(150);
const MAX_BROWSER_KEYS: usize = 64;
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    schema_version: u8,
    browser_keys: BTreeSet<String>,
    #[serde(default)]
    capability_version: Option<u8>,
    #[serde(default)]
    capability: Option<Capability>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub v: u8,
    pub machine_id: String,
    pub public_key: String,
    pub secret: String,
    pub expires_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
}
struct Reservation {
    session: String,
    deadline: Instant,
}
pub struct Pairings {
    path: PathBuf,
    capability_path: PathBuf,
    keys: BTreeSet<String>,
    key_epochs: BTreeMap<String, u64>,
    pub capability: Capability,
    reserved: Option<Reservation>,
    // Every manual rotation invalidates all in-flight authentication, even for
    // a key later re-enrolled. Existing sessions instead track key membership.
    generation: u64,
    changed: tokio::sync::watch::Sender<u64>,
}
impl Pairings {
    pub fn load(dir: &Path, machine_id: String, public_key: String, reset: bool) -> Result<Self> {
        relay_crypto::ensure_private_dir(dir)?;
        let path = dir.join("pairings.json");
        let mut keys = BTreeSet::new();
        let mut capability = None;
        if !reset && path.exists() {
            let stored: Stored =
                serde_json::from_slice(&relay_crypto::read_private(&path, 65536)?)?;
            ensure!(
                matches!(stored.schema_version, 1 | 2)
                    && stored.browser_keys.len() <= MAX_BROWSER_KEYS,
                "invalid pairing state"
            );
            for key in &stored.browser_keys {
                relay_crypto::machine_id(key)?;
            }
            keys = stored.browser_keys;
            // Keep the outer v1 envelope readable by installed rollback binaries.
            // Only the explicit policy marker (or the early development v2
            // envelope) can assert permanent access. Unmarked v1 is migrated once.
            match (stored.schema_version, stored.capability_version) {
                (1, None) => {}
                (1 | 2, Some(2)) | (2, None) => {
                    capability = Some(
                        stored
                            .capability
                            .ok_or_else(|| anyhow::anyhow!("missing permanent capability"))?,
                    );
                }
                _ => anyhow::bail!("unsupported pairing capability policy"),
            }
        }
        let capability = capability.unwrap_or_else(|| Capability {
            v: 1,
            machine_id: machine_id.clone(),
            public_key: public_key.clone(),
            secret: random_token(32),
            expires_at: PERMANENT_EXPIRY,
            channel: None,
            base_path: None,
        });
        ensure!(
            capability.machine_id == machine_id
                && capability.public_key == public_key
                && capability.v == 1
                && capability.expires_at == PERMANENT_EXPIRY,
            "invalid capability identity"
        );
        relay_crypto::decode(&capability.secret, 32)?;
        let (changed, _) = tokio::sync::watch::channel(0);
        let this = Self {
            path,
            capability_path: dir.join("pairing.json"),
            key_epochs: keys.iter().map(|key| (key.clone(), 0)).collect(),
            keys,
            capability,
            reserved: None,
            generation: 0,
            changed,
        };
        this.persist()?;
        this.publish()?;
        Ok(this)
    }
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn available(&self) -> bool {
        !self.at_capacity()
    }
    pub fn at_capacity(&self) -> bool {
        self.keys.len() >= MAX_BROWSER_KEYS
    }
    fn publish(&self) -> Result<()> {
        if self.at_capacity() {
            match std::fs::remove_file(&self.capability_path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        } else {
            let bytes = serde_json::to_vec(&self.capability)?;
            if self.capability_path.exists()
                && relay_crypto::read_private(&self.capability_path, 4096)? == bytes
            {
                return Ok(());
            }
            relay_crypto::write_private(&self.capability_path, &bytes)
        }
    }
    pub fn set_channel(&mut self, main: bool) -> Result<()> {
        self.capability.channel = main.then(|| "main".into());
        self.capability.base_path = main.then(|| "/main/".into());
        self.persist()?;
        self.publish()
    }
    pub fn refresh(&mut self) -> Result<bool> {
        if !self.reservation_live() {
            self.reserved = None;
        }
        self.publish()?;
        Ok(false)
    }
    fn reservation_live(&self) -> bool {
        self.reserved
            .as_ref()
            .is_some_and(|r| Instant::now() < r.deadline)
    }
    fn persist(&self) -> Result<()> {
        relay_crypto::write_private(
            &self.path,
            &serde_json::to_vec(&Stored {
                schema_version: 1,
                capability_version: Some(2),
                browser_keys: self.keys.clone(),
                capability: Some(self.capability.clone()),
            })?,
        )
    }
    pub fn authorized_owners(&self) -> Vec<String> {
        self.keys.iter().cloned().collect()
    }
    pub fn known(&self, key: &str) -> bool {
        self.keys.contains(key)
    }
    /// Re-enrollment cannot resurrect an older live session of a revoked key.
    pub fn authorized_at(&self, key: &str, generation: u64) -> bool {
        self.known(key)
            && self
                .key_epochs
                .get(key)
                .is_some_and(|epoch| *epoch <= generation)
    }
    /// Atomic authoritative commit; pairing.json is only a derived display fixture.
    pub fn rotate(&mut self, caller: &str) -> Result<()> {
        ensure!(self.known(caller), "browser authorization revoked");
        let old_keys = self.keys.clone();
        let old = self.capability.clone();
        self.keys.retain(|key| key == caller);
        self.capability.secret = random_token(32);
        if let Err(e) = self.persist() {
            self.keys = old_keys;
            self.capability = old;
            return Err(e);
        }
        self.reserved = None;
        self.key_epochs.retain(|key, _| key == caller);
        self.generation += 1;
        self.changed.send_replace(self.generation);
        // Never roll back committed revocation if updating the display fails.
        let _ = self.publish();
        Ok(())
    }
    pub fn reserve(&mut self, session: &str, transcript: &[u8], proof: &str) -> Result<()> {
        ensure!(
            !self.at_capacity() && !self.reservation_live(),
            "pairing capability unavailable"
        );
        relay_crypto::verify_pair_proof(&self.capability.secret, transcript, proof)?;
        self.reserved = Some(Reservation {
            session: session.into(),
            deadline: Instant::now() + RESERVATION_TIMEOUT,
        });
        Ok(())
    }
    pub fn release(&mut self, session: &str) {
        if self.reserved.as_ref().is_some_and(|r| r.session == session) {
            self.reserved = None;
        }
    }
    pub fn complete(&mut self, session: &str, key: &str, pairing: bool) -> Result<()> {
        if !pairing {
            ensure!(self.known(key), "unknown browser key");
            return Ok(());
        }
        ensure!(
            self.reservation_live() && self.reserved.as_ref().is_some_and(|r| r.session == session),
            "pairing ownership lost"
        );
        ensure!(
            !self.at_capacity() || self.known(key),
            "pairing key limit reached"
        );
        let inserted = self.keys.insert(key.into());
        if let Err(e) = self.persist() {
            if inserted {
                self.keys.remove(key);
            }
            return Err(e);
        }
        if inserted {
            self.key_epochs.insert(key.into(), self.generation);
        }
        self.reserved = None;
        let _ = self.publish();
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn directory() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }
    fn enroll(p: &mut Pairings, key: &str) {
        let proof = relay_crypto::pair_proof(&p.capability.secret, b"auth").unwrap();
        p.reserve("s", b"auth", &proof).unwrap();
        p.complete("s", key, true).unwrap();
    }
    fn key(dir: &Path, name: &str) -> String {
        relay_crypto::Identity::load_or_create(&dir.join(name))
            .unwrap()
            .public_key()
    }
    #[test]
    fn reusable_persistent_capability_and_rotation() {
        let dir = directory();
        let mut p = Pairings::load(dir.path(), "m".into(), "p".into(), false).unwrap();
        let a = key(dir.path(), "a");
        let b = key(dir.path(), "b");
        let original = p.capability.secret.clone();
        enroll(&mut p, &a);
        enroll(&mut p, &b);
        assert_eq!(p.capability.secret, original);
        assert!(!p.refresh().unwrap());
        drop(p);
        let mut p = Pairings::load(dir.path(), "m".into(), "p".into(), false).unwrap();
        assert_eq!(p.capability.secret, original);
        assert!(p.known(&a) && p.known(&b));
        let proof = relay_crypto::pair_proof(&original, b"auth").unwrap();
        p.reserve("pending", b"auth", &proof).unwrap();
        p.rotate(&a).unwrap();
        assert!(p.known(&a) && !p.known(&b));
        assert!(p.rotate(&b).is_err());
        assert!(p.complete("pending", &b, true).is_err());
        assert!(p.reserve("old", b"auth", &proof).is_err());
        drop(p);
        let p = Pairings::load(dir.path(), "m".into(), "p".into(), false).unwrap();
        assert!(p.known(&a) && !p.known(&b));
        assert_ne!(p.capability.secret, original);
    }
    #[test]
    fn revoked_key_reenrollment_does_not_resurrect_old_sessions() {
        let dir = directory();
        let mut p = Pairings::load(dir.path(), "m".into(), "p".into(), false).unwrap();
        let a = key(dir.path(), "a");
        let b = key(dir.path(), "b");
        enroll(&mut p, &a);
        enroll(&mut p, &b);
        let old_epoch = p.generation();
        p.rotate(&a).unwrap();
        enroll(&mut p, &b);
        assert!(p.authorized_at(&a, old_epoch));
        assert!(!p.authorized_at(&b, old_epoch));
        assert!(p.authorized_at(&b, p.generation()));
    }
    #[test]
    fn failed_commit_preserves_authorizations_secret_and_reservation() {
        let dir = directory();
        let mut p = Pairings::load(dir.path(), "m".into(), "p".into(), false).unwrap();
        let a = key(dir.path(), "a");
        let b = key(dir.path(), "b");
        enroll(&mut p, &a);
        enroll(&mut p, &b);
        let secret = p.capability.secret.clone();
        let proof = relay_crypto::pair_proof(&secret, b"auth").unwrap();
        p.reserve("pending", b"auth", &proof).unwrap();
        std::fs::set_permissions(&p.path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(p.rotate(&a).is_err());
        assert_eq!(p.capability.secret, secret);
        assert!(p.known(&b));
        assert_eq!(p.generation(), 0);
        std::fs::set_permissions(&p.path, std::fs::Permissions::from_mode(0o600)).unwrap();
        p.complete("pending", &b, true).unwrap();
    }
    #[test]
    fn rotation_isolated_by_connector_state_and_channel() {
        let a_dir = directory();
        let b_dir = directory();
        let mut a = Pairings::load(a_dir.path(), "m".into(), "p".into(), false).unwrap();
        let mut b = Pairings::load(b_dir.path(), "m".into(), "p".into(), false).unwrap();
        a.set_channel(false).unwrap();
        b.set_channel(true).unwrap();
        let owner = key(a_dir.path(), "owner");
        enroll(&mut a, &owner);
        enroll(&mut b, &owner);
        let b_secret = b.capability.secret.clone();
        a.rotate(&owner).unwrap();
        assert_eq!(b.capability.secret, b_secret);
        assert!(b.known(&owner));
        assert_eq!(b.generation(), 0);
    }
    #[test]
    fn canonical_envelope_remains_readable_by_legacy_connector_after_rotation() {
        #[derive(Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LegacyStored {
            schema_version: u8,
            browser_keys: BTreeSet<String>,
        }
        let read_legacy = |path: &Path| {
            let legacy: LegacyStored =
                serde_json::from_slice(&relay_crypto::read_private(path, 65536).unwrap()).unwrap();
            assert_eq!(legacy.schema_version, 1);
            legacy
        };
        let dir = directory();
        let mut p = Pairings::load(dir.path(), "m".into(), "p".into(), false).unwrap();
        let a = key(dir.path(), "a");
        let b = key(dir.path(), "b");
        enroll(&mut p, &a);
        enroll(&mut p, &b);
        let legacy = read_legacy(&p.path);
        assert_eq!(legacy.browser_keys, BTreeSet::from([a.clone(), b.clone()]));
        p.rotate(&a).unwrap();
        let legacy = read_legacy(&p.path);
        assert_eq!(legacy.browser_keys, BTreeSet::from([a.clone()]));
        let secret = p.capability.secret.clone();
        let path = p.path.clone();
        drop(p);
        let p = Pairings::load(dir.path(), "m".into(), "p".into(), false).unwrap();
        assert_eq!(p.capability.secret, secret);
        assert!(!p.known(&b));
        drop(p);
        // A rollback writer strips unknown policy fields, but persists only the
        // currently trusted keys. The next upgrade renews access once, safely.
        relay_crypto::write_private(&path, &serde_json::to_vec(&legacy).unwrap()).unwrap();
        let p = Pairings::load(dir.path(), "m".into(), "p".into(), false).unwrap();
        assert_ne!(p.capability.secret, secret);
        assert!(p.known(&a) && !p.known(&b));
        read_legacy(&path);
    }
    #[test]
    fn unknown_capability_policy_is_rejected_without_rewriting_state() {
        let dir = directory();
        let p = Pairings::load(dir.path(), "m".into(), "p".into(), false).unwrap();
        let mut stored: serde_json::Value =
            serde_json::from_slice(&relay_crypto::read_private(&p.path, 65536).unwrap()).unwrap();
        assert_eq!(stored["schemaVersion"], 1);
        assert_eq!(stored["capabilityVersion"], 2);
        stored["capabilityVersion"] = serde_json::json!(3);
        let bytes = serde_json::to_vec(&stored).unwrap();
        relay_crypto::write_private(&p.path, &bytes).unwrap();
        assert!(Pairings::load(dir.path(), "m".into(), "p".into(), false).is_err());
        assert_eq!(relay_crypto::read_private(&p.path, 65536).unwrap(), bytes);
    }
    #[test]
    fn legacy_keys_preserved_but_expired_capability_never_revived() {
        let dir = directory();
        relay_crypto::ensure_private_dir(dir.path()).unwrap();
        let a = key(dir.path(), "a");
        relay_crypto::write_private(
            &dir.path().join("pairings.json"),
            &serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"browserKeys":[a]})).unwrap(),
        )
        .unwrap();
        let expired = Capability {
            v: 1,
            machine_id: "m".into(),
            public_key: "p".into(),
            secret: random_token(32),
            expires_at: 1,
            channel: None,
            base_path: None,
        };
        relay_crypto::write_private(
            &dir.path().join("pairing.json"),
            &serde_json::to_vec(&expired).unwrap(),
        )
        .unwrap();
        let mut p = Pairings::load(dir.path(), "m".into(), "p".into(), false).unwrap();
        assert!(p.known(&a));
        assert_eq!(p.capability.expires_at, PERMANENT_EXPIRY);
        assert_ne!(p.capability.secret, expired.secret);
        let proof = relay_crypto::pair_proof(&expired.secret, b"auth").unwrap();
        assert!(p.reserve("expired", b"auth", &proof).is_err());
    }
}
