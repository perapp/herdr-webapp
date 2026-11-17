//! Pairing/signaling primitives and private connector-state files. Never log secrets.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use p256::ecdsa::{
    Signature, SigningKey, VerifyingKey,
    signature::{Signer, Verifier},
};
use p256::elliptic_curve::rand_core::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
};

pub const PROTOCOL_VERSION: u32 = 1;
// Legacy wire/package identity; display provenance lives in build_info.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn random_token(bytes: usize) -> String {
    use rand::RngCore;
    let mut value = vec![0; bytes];
    rand::rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}
pub fn decode(value: &str, length: usize) -> Result<Vec<u8>> {
    ensure!(value.len() <= length * 2 + 8, "Invalid encoded value");
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .context("Invalid encoded value")?;
    ensure!(
        bytes.len() == length && URL_SAFE_NO_PAD.encode(&bytes) == value,
        "Invalid encoded value"
    );
    Ok(bytes)
}
pub fn machine_id(public_key: &str) -> Result<String> {
    let bytes = decode(public_key, 65)?;
    VerifyingKey::from_sec1_bytes(&bytes).context("Invalid public key")?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
}
pub fn verify(public_key: &str, message: &[u8], signature: &str) -> Result<()> {
    let key =
        VerifyingKey::from_sec1_bytes(&decode(public_key, 65)?).context("Invalid public key")?;
    let signature = Signature::from_slice(&decode(signature, 64)?).context("Invalid signature")?;
    key.verify(message, &signature)
        .context("Signature rejected")
}
#[cfg(test)]
pub fn pair_proof(secret: &str, message: &[u8]) -> Result<String> {
    let mut mac = Hmac::<sha2_10::Sha256>::new_from_slice(&decode(secret, 32)?)?;
    mac.update(message);
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}
pub fn verify_pair_proof(secret: &str, message: &[u8], proof: &str) -> Result<()> {
    let mut mac = Hmac::<sha2_10::Sha256>::new_from_slice(&decode(secret, 32)?)?;
    mac.update(message);
    mac.verify_slice(&decode(proof, 32)?)
        .context("Pairing proof rejected")
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SavedIdentity {
    schema_version: u32,
    private_key: String,
}
pub struct Identity {
    key: SigningKey,
}
impl Identity {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        ensure_private_dir(path.parent().context("Identity parent missing")?)?;
        if fs::symlink_metadata(path).is_err() {
            let key = SigningKey::random(&mut OsRng);
            let bytes = serde_json::to_vec(&SavedIdentity {
                schema_version: 1,
                private_key: URL_SAFE_NO_PAD.encode(key.to_bytes()),
            })?;
            // Creation is exclusive: never rotate an existing laptop identity.
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
            {
                Ok(mut file) => {
                    file.write_all(&bytes)?;
                    file.sync_all()?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        let saved: SavedIdentity = serde_json::from_slice(&read_private(path, 4096)?)?;
        ensure!(saved.schema_version == 1, "Unsupported identity schema");
        let key =
            SigningKey::from_slice(&decode(&saved.private_key, 32)?).context("Invalid identity")?;
        Ok(Self { key })
    }
    /// Process-lifetime key that is never written anywhere.
    pub fn ephemeral() -> Self {
        Self {
            key: SigningKey::random(&mut OsRng),
        }
    }
    pub fn public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.key.verifying_key().to_encoded_point(false).as_bytes())
    }
    pub fn machine_id(&self) -> String {
        machine_id(&self.public_key()).expect("generated public key")
    }
    pub fn sign(&self, message: &[u8]) -> String {
        let signature: Signature = self.key.sign(message);
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    }
}
fn private_metadata(metadata: &fs::Metadata, directory: bool) -> Result<()> {
    ensure!(
        metadata.uid() == unsafe { libc::getuid() }
            && metadata.mode() & 0o077 == 0
            && !metadata.file_type().is_symlink(),
        "State must be private and owned by this user"
    );
    ensure!(
        if directory {
            metadata.is_dir()
        } else {
            metadata.is_file() && metadata.nlink() == 1
        },
        "Unsafe state entry"
    );
    Ok(())
}
pub fn ensure_private_dir(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "State directory must be absolute");
    ensure!(
        !path.components().any(|p| matches!(
            p,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )),
        "State path must be normalized"
    );
    if fs::symlink_metadata(path).is_err() {
        let parent = path.parent().context("State parent missing")?;
        if !parent.exists() {
            ensure_private_dir(parent)?;
        }
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(e)
                }
            })?;
    }
    private_metadata(&fs::symlink_metadata(path)?, true)?;
    ensure!(
        fs::canonicalize(path)? == path,
        "State path must not use symlinks"
    );
    Ok(())
}
/// Hold this file for the lifetime of the connector, including reconnects.
/// Separate from the launcher's supervisor lock so its own child can acquire it.
pub fn lock_private(path: &Path) -> Result<fs::File> {
    use std::os::fd::AsRawFd;
    ensure_private_dir(path.parent().context("Lock parent missing")?)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    private_metadata(&file.metadata()?, false)?;
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "Another connector is using this state directory; stop it before starting another"
    );
    Ok(file)
}
pub fn read_private(path: &Path, max: usize) -> Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    private_metadata(&metadata, false)?;
    ensure!(metadata.len() <= max as u64, "State file exceeds limit");
    let mut bytes = Vec::new();
    (&mut file).take((max + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "State file exceeds limit");
    Ok(bytes)
}
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("State parent missing")?;
    ensure_private_dir(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        private_metadata(&metadata, false)?;
    }
    let staged = parent.join(format!(".state-{}", random_token(16)));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&staged)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&staged, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(staged);
    }
    result
}
/// Validate and normalize a configured trust origin; never infer it from Host.
pub fn origin(value: &str, allow_http_loopback: bool) -> Result<String> {
    ensure!(
        value.len() <= 2048 && !value.chars().any(char::is_control),
        "Invalid relay origin"
    );
    let parsed = url::Url::parse(value).context("Invalid relay origin")?;
    ensure!(
        parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.path() == "/",
        "Expected an origin without credentials, path, query or fragment"
    );
    let host = parsed.host_str().context("Relay hostname missing")?;
    let loopback = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    ensure!(
        parsed.scheme() == "https"
            || (parsed.scheme() == "http" && allow_http_loopback && loopback),
        "HTTPS required (HTTP development is loopback-only)"
    );
    let normalized = parsed.origin().ascii_serialization();
    ensure!(
        value == normalized || value == format!("{normalized}/"),
        "Relay origin must be canonical"
    );
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn scratch() -> tempfile::TempDir {
        let dir = tempfile::Builder::new()
            .tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap())
            .unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }
    #[test]
    fn persistent_identity_and_raw_signatures() {
        let d = scratch();
        let path = d.path().join("identity.json");
        let first = Identity::load_or_create(&path).unwrap();
        let next = Identity::load_or_create(&path).unwrap();
        assert_eq!(first.machine_id(), next.machine_id());
        let signature = first.sign(b"transcript");
        assert_eq!(decode(&signature, 64).unwrap().len(), 64);
        verify(&next.public_key(), b"transcript", &signature).unwrap();
        assert!(verify(&next.public_key(), b"tampered", &signature).is_err());
        assert!(verify(&random_token(65), b"transcript", &signature).is_err());
    }
    #[test]
    fn proofs_are_bound_to_transcript_and_secret() {
        let secret = random_token(32);
        let proof = pair_proof(&secret, b"session-one").unwrap();
        verify_pair_proof(&secret, b"session-one", &proof).unwrap();
        assert!(verify_pair_proof(&secret, b"session-two", &proof).is_err());
        assert!(verify_pair_proof(&random_token(32), b"session-one", &proof).is_err());
    }
    #[test]
    fn private_files_reject_links_modes_and_oversize() {
        let d = scratch();
        let file = d.path().join("state");
        write_private(&file, b"secret").unwrap();
        assert!(read_private(&file, 2).is_err());
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(read_private(&link, 64).is_err());
        assert!(write_private(&link, b"bad").is_err());
        fs::remove_file(link).unwrap();
        fs::hard_link(&file, d.path().join("hard")).unwrap();
        assert!(read_private(&file, 64).is_err());
        fs::remove_file(d.path().join("hard")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private(&file, 64).is_err());
    }
    #[test]
    fn connector_state_is_exclusive_and_released_on_exit() {
        let d = scratch();
        let path = d.path().join("connector.lock");
        let guard = lock_private(&path).unwrap();
        assert!(lock_private(&path).is_err());
        drop(guard);
        assert!(lock_private(&path).is_ok());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(lock_private(&path).is_err());
    }
    #[test]
    fn origin_trust_boundary() {
        assert_eq!(
            origin("https://example.test", false).unwrap(),
            "https://example.test"
        );
        assert!(origin("http://example.test", true).is_err());
        assert!(origin("http://127.0.0.1:9001", false).is_err());
        assert!(origin("http://127.0.0.1:9001", true).is_ok());
        for value in [
            "https://user:pw@example.test",
            "https://example.test/path",
            "https://example.test/?secret=1",
            "https://example.test/#secret",
            "https://example.test\n",
        ] {
            assert!(origin(value, false).is_err());
        }
    }
}
