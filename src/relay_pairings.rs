//! Private, serialized pairing capability and browser-public-key ownership.
use crate::relay_crypto::{self, random_token};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    schema_version: u8,
    browser_keys: BTreeSet<String>,
}
const CAPABILITY_SECONDS: u64 = 300;
const RESERVATION_TIMEOUT: Duration = Duration::from_secs(150);
const MAX_BROWSER_KEYS: usize = 64;

#[derive(Clone, Serialize)]
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
    pub capability: Capability,
    reserved: Option<Reservation>,
    consumed: bool,
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl Pairings {
    pub fn load(dir: &Path, machine_id: String, public_key: String, reset: bool) -> Result<Self> {
        relay_crypto::ensure_private_dir(dir)?;
        let path = dir.join("pairings.json");
        let keys = if reset || !path.exists() {
            BTreeSet::new()
        } else {
            let stored: Stored =
                serde_json::from_slice(&relay_crypto::read_private(&path, 65536)?)?;
            ensure!(
                stored.schema_version == 1 && stored.browser_keys.len() <= 64,
                "invalid pairing state"
            );
            for key in &stored.browser_keys {
                relay_crypto::machine_id(key)?;
            }
            stored.browser_keys
        };
        let this = Self {
            path,
            capability_path: dir.join("pairing.json"),
            keys,
            capability: Capability {
                v: 1,
                machine_id,
                public_key,
                secret: random_token(32),
                expires_at: now() + CAPABILITY_SECONDS,
                channel: None,
                base_path: None,
            },
            reserved: None,
            consumed: false,
        };
        this.persist()?;
        if this.at_capacity() {
            this.remove_capability()?;
        } else {
            this.persist_capability(&this.capability)?;
        }
        Ok(this)
    }
    pub fn available(&self) -> bool {
        !self.at_capacity()
            && !self.consumed
            && now() < self.capability.expires_at
            && !self.reservation_live()
    }
    pub fn at_capacity(&self) -> bool {
        self.keys.len() >= MAX_BROWSER_KEYS
    }
    fn remove_capability(&self) -> Result<()> {
        match std::fs::remove_file(&self.capability_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    fn persist_capability(&self, capability: &Capability) -> Result<()> {
        relay_crypto::write_private(&self.capability_path, &serde_json::to_vec(capability)?)
    }
    /// Persist channel metadata with every subsequent generation, not just startup.
    pub fn set_channel(&mut self, main: bool) -> Result<()> {
        let mut capability = self.capability.clone();
        capability.channel = main.then(|| "main".into());
        capability.base_path = main.then(|| "/main/".into());
        if !self.at_capacity() && !self.consumed {
            self.persist_capability(&capability)?;
        }
        self.capability = capability;
        Ok(())
    }
    fn reservation_live(&self) -> bool {
        self.reserved
            .as_ref()
            .is_some_and(|r| Instant::now() < r.deadline)
    }
    /// Caller holds the pairing mutex. Commit the new secret only after its atomic
    /// private-file replacement succeeds; retries must never publish phantom codes.
    pub fn refresh(&mut self) -> Result<bool> {
        self.refresh_at(now())
    }
    fn refresh_at(&mut self, timestamp: u64) -> Result<bool> {
        if self.reserved.is_some() && !self.reservation_live() {
            self.reserved = None;
        }
        if self.at_capacity() {
            self.remove_capability()?;
            return Ok(false);
        }
        if self.reservation_live() || (!self.consumed && timestamp < self.capability.expires_at) {
            return Ok(false);
        }
        let mut capability = self.capability.clone();
        capability.secret = random_token(32);
        capability.expires_at = timestamp + CAPABILITY_SECONDS;
        self.persist_capability(&capability)?;
        self.capability = capability;
        self.consumed = false;
        Ok(true)
    }
    fn persist(&self) -> Result<()> {
        relay_crypto::write_private(
            &self.path,
            &serde_json::to_vec(&Stored {
                schema_version: 1,
                browser_keys: self.keys.clone(),
            })?,
        )
    }
    pub fn known(&self, key: &str) -> bool {
        self.keys.contains(key)
    }
    pub fn reserve(&mut self, session: &str, transcript: &[u8], proof: &str) -> Result<()> {
        ensure!(
            !self.at_capacity()
                && !self.consumed
                && now() < self.capability.expires_at
                && !self.reservation_live(),
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
    /// Caller holds the shared pairing mutex throughout validation and persistence.
    pub fn complete(&mut self, session: &str, key: &str, pairing: bool) -> Result<()> {
        if !pairing {
            ensure!(self.known(key), "unknown browser key");
            return Ok(());
        }
        ensure!(
            !self.consumed
                && self.reservation_live()
                && self.reserved.as_ref().is_some_and(|r| r.session == session),
            "pairing ownership lost"
        );
        if self.at_capacity() && !self.keys.contains(key) {
            bail!("pairing key limit reached");
        }
        let inserted = self.keys.insert(key.into());
        if let Err(error) = self.persist() {
            if inserted {
                self.keys.remove(key);
            }
            return Err(error);
        }
        self.consumed = true;
        self.reserved = None;
        // The secret is no longer useful; do not leave an apparently live QR fixture.
        let _ = std::fs::remove_file(&self.capability_path);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn fixture() -> (tempfile::TempDir, Pairings) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let p = Pairings::load(dir.path(), "machine".into(), "public".into(), false).unwrap();
        (dir, p)
    }
    fn reserve(p: &mut Pairings, session: &str) {
        let proof = relay_crypto::pair_proof(&p.capability.secret, b"auth").unwrap();
        p.reserve(session, b"auth", &proof).unwrap();
    }

    #[test]
    fn expiry_rotates_once_preserving_metadata_and_private_atomic_file() {
        let (dir, mut p) = fixture();
        p.set_channel(true).unwrap();
        let original = p.capability.clone();
        let previous = std::fs::File::open(&p.capability_path).unwrap();
        assert!(!p.refresh_at(original.expires_at - 1).unwrap());
        assert!(p.refresh_at(original.expires_at).unwrap());
        assert_ne!(p.capability.secret, original.secret);
        assert_eq!(
            p.capability.expires_at,
            original.expires_at + CAPABILITY_SECONDS
        );
        assert_eq!(p.capability.machine_id, original.machine_id);
        assert_eq!(p.capability.public_key, original.public_key);
        assert!(!p.refresh_at(original.expires_at + 1).unwrap());
        let file = dir.path().join("pairing.json");
        let value: serde_json::Value =
            serde_json::from_slice(&relay_crypto::read_private(&file, 4096).unwrap()).unwrap();
        assert_eq!(value["channel"], "main");
        assert_eq!(value["basePath"], "/main/");
        assert_eq!(value["secret"], p.capability.secret);
        let metadata = std::fs::metadata(file).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), unsafe { libc::getuid() });
        assert_ne!(metadata.ino(), previous.metadata().unwrap().ino());
    }

    #[test]
    fn live_reservation_survives_expiry_but_stale_reservation_is_bounded() {
        let (_dir, mut p) = fixture();
        reserve(&mut p, "live");
        let original = p.capability.secret.clone();
        p.capability.expires_at = now();
        assert!(!p.refresh().unwrap());
        assert_eq!(p.capability.secret, original);
        p.complete("live", "trusted", true).unwrap();
        assert!(p.refresh().unwrap());
        assert!(p.known("trusted"));
        p.complete("resume", "trusted", false).unwrap();
        reserve(&mut p, "stale");
        p.reserved.as_mut().unwrap().deadline = Instant::now() - Duration::from_secs(1);
        p.capability.expires_at = now();
        assert!(p.complete("stale", "intruder", true).is_err());
        assert!(p.refresh().unwrap());
        assert!(!p.known("intruder"));
        reserve(&mut p, "next");
        p.release("stale");
        p.complete("next", "next-browser", true).unwrap();
    }

    #[test]
    fn failed_proof_and_released_reservation_do_not_rotate_valid_capability() {
        let (_dir, mut p) = fixture();
        let original = p.capability.secret.clone();
        assert!(p.reserve("bad", b"auth", "bad proof").is_err());
        reserve(&mut p, "first");
        p.release("first");
        assert!(!p.refresh().unwrap());
        assert_eq!(p.capability.secret, original);
        reserve(&mut p, "retry");
        p.complete("retry", "trusted", true).unwrap();
        assert!(p.refresh().unwrap());
        let old_proof = relay_crypto::pair_proof(&original, b"auth").unwrap();
        assert!(p.reserve("old", b"auth", &old_proof).is_err());
        assert!(p.known("trusted"));
    }

    #[test]
    fn failed_rotation_does_not_publish_secret_and_retries_after_repair() {
        let (_dir, mut p) = fixture();
        p.capability.expires_at = now();
        let original = p.capability.secret.clone();
        std::fs::set_permissions(&p.capability_path, std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(p.refresh().is_err());
        assert_eq!(p.capability.secret, original);
        std::fs::set_permissions(&p.capability_path, std::fs::Permissions::from_mode(0o600))
            .unwrap();
        assert!(p.refresh().unwrap());
        assert_ne!(p.capability.secret, original);
    }

    #[test]
    fn persistence_failure_preserves_reservation_and_does_not_trust_browser() {
        let (_dir, mut p) = fixture();
        reserve(&mut p, "owner");
        std::fs::set_permissions(&p.path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(p.complete("owner", "new", true).is_err());
        assert!(!p.known("new"));
        assert!(!p.refresh().unwrap());
        std::fs::set_permissions(&p.path, std::fs::Permissions::from_mode(0o600)).unwrap();
        p.complete("owner", "new", true).unwrap();
        assert!(p.refresh().unwrap());
    }

    #[test]
    fn capacity_removes_fixture_without_rotating_or_revoking_trust() {
        let (_dir, mut p) = fixture();
        p.keys = (0..MAX_BROWSER_KEYS).map(|n| format!("key-{n}")).collect();
        let original = p.capability.secret.clone();
        assert!(!p.refresh().unwrap());
        assert!(!p.capability_path.exists());
        assert!(!p.available());
        assert_eq!(p.capability.secret, original);
        p.complete("resume", "key-0", false).unwrap();
        let proof = relay_crypto::pair_proof(&original, b"auth").unwrap();
        assert!(p.reserve("new", b"auth", &proof).is_err());
        assert!(p.complete("resume", "unknown", false).is_err());
    }

    #[tokio::test]
    async fn concurrent_rotation_and_completion_serialize_without_revoking_known_keys() {
        let (_dir, mut p) = fixture();
        reserve(&mut p, "owner");
        p.capability.expires_at = now();
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(p));
        let rotating = shared.clone();
        let completing = shared.clone();
        let (rotation, completion) =
            tokio::join!(async move { rotating.lock().await.refresh() }, async move {
                completing.lock().await.complete("owner", "trusted", true)
            });
        rotation.unwrap();
        completion.unwrap();
        let mut p = shared.lock().await;
        p.refresh().unwrap();
        assert!(p.available());
        p.complete("resume", "trusted", false).unwrap();
        assert!(p.complete("owner", "intruder", true).is_err());
    }
    #[test]
    fn capability_ownership_is_exclusive_and_consumed_once() {
        let dir = tempfile::Builder::new()
            .tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
            .unwrap();
        std::fs::set_permissions(
            dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let mut p = Pairings::load(dir.path(), "machine".into(), "public".into(), true).unwrap();
        let browser = relay_crypto::Identity::load_or_create(&dir.path().join("browser-test.json"))
            .unwrap()
            .public_key();
        let original_secret = p.capability.secret.clone();
        let proof = relay_crypto::pair_proof(&p.capability.secret, b"auth").unwrap();
        p.reserve("a", b"auth", &proof).unwrap();
        assert!(p.reserve("b", b"auth", &proof).is_err());
        p.release("b");
        assert!(p.complete("b", &browser, true).is_err());
        p.complete("a", &browser, true).unwrap();
        assert!(p.reserve("b", b"auth", &proof).is_err());
        assert!(p.known(&browser));
        assert!(p.refresh().unwrap());
        assert!(p.reserve("b", b"auth", &proof).is_err());
        assert!(p.known(&browser));
        let loaded = Pairings::load(dir.path(), "machine".into(), "public".into(), false).unwrap();
        assert!(loaded.known(&browser));
        assert_ne!(loaded.capability.secret, original_secret);
        let reset = Pairings::load(dir.path(), "machine".into(), "public".into(), true).unwrap();
        assert!(!reset.known(&browser));
    }
}
