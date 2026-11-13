//! Private, serialized pairing capability and browser-public-key ownership.
use crate::relay_crypto::{self, random_token};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    schema_version: u8,
    browser_keys: BTreeSet<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub v: u8,
    pub machine_id: String,
    pub public_key: String,
    pub secret: String,
    pub expires_at: u64,
}
pub struct Pairings {
    path: PathBuf,
    capability_path: PathBuf,
    keys: BTreeSet<String>,
    pub capability: Capability,
    reserved: Option<String>,
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
                expires_at: now() + 300,
            },
            reserved: None,
            consumed: false,
        };
        this.persist()?;
        relay_crypto::write_private(
            &this.capability_path,
            &serde_json::to_vec(&this.capability)?,
        )?;
        Ok(this)
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
            !self.consumed && now() < self.capability.expires_at && self.reserved.is_none(),
            "pairing capability unavailable"
        );
        relay_crypto::verify_pair_proof(&self.capability.secret, transcript, proof)?;
        self.reserved = Some(session.into());
        Ok(())
    }
    pub fn release(&mut self, session: &str) {
        if self.reserved.as_deref() == Some(session) {
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
                && now() < self.capability.expires_at
                && self.reserved.as_deref() == Some(session),
            "pairing ownership lost"
        );
        if self.keys.len() >= 64 && !self.keys.contains(key) {
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
        let loaded = Pairings::load(dir.path(), "machine".into(), "public".into(), false).unwrap();
        assert!(loaded.known(&browser));
        assert_ne!(loaded.capability.secret, original_secret);
        let reset = Pairings::load(dir.path(), "machine".into(), "public".into(), true).unwrap();
        assert!(!reset.known(&browser));
    }
}
