//! Short-lived, connector-verifiable enterprise authorization and the audit trail.
//!
//! The relay (the authorization service) signs a grant only after it has verified
//! the browser's Okta session and that its principal equals the enrolled owner of
//! the computer. A grant names one principal, one computer, one browser key, one
//! relay context and one signaling session, so it cannot be reused elsewhere.
use crate::{enterprise_oidc::Principal, relay_crypto};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PREFIX: &str = "herdr-enterprise-grant-v1\n";
/// Wall clocks only reject stale grants; validity is then timed monotonically.
pub const CLOCK_TOLERANCE: u64 = 120;
pub const MAX_LIFETIME: u64 = 900;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Grant {
    pub v: u8,
    pub iss: String,
    pub sub: String,
    pub machine_id: String,
    pub browser_key: String,
    /// Relay origin, including the channel path used in signaling transcripts.
    pub context: String,
    pub session_id: String,
    pub iat: u64,
    pub exp: u64,
    pub jti: String,
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
impl Grant {
    pub fn new(
        principal: &Principal,
        machine_id: &str,
        browser_key: &str,
        context: &str,
        session_id: &str,
        lifetime: u64,
    ) -> Self {
        let iat = now();
        Self {
            v: 1,
            iss: principal.iss.clone(),
            sub: principal.sub.clone(),
            machine_id: machine_id.into(),
            browser_key: browser_key.into(),
            context: context.into(),
            session_id: session_id.into(),
            iat,
            exp: iat + lifetime.min(MAX_LIFETIME),
            jti: relay_crypto::random_token(16),
        }
    }
    pub fn sign(&self, key: &relay_crypto::Identity) -> Value {
        let body = serde_json::to_string(self).expect("grant serializes");
        let signature = key.sign(format!("{PREFIX}{body}").as_bytes());
        json!({"body": body, "signature": signature})
    }
}
/// Everything the connector already knows independently of the relay message.
pub struct Expected<'a> {
    pub key: &'a str,
    pub owner: &'a Principal,
    pub machine_id: &'a str,
    pub context: &'a str,
    pub session_id: &'a str,
    /// Renewals must keep the browser key of the first grant.
    pub browser_key: Option<&'a str>,
}
/// Returns the grant and how long it may be relied on from this moment.
pub fn verify(value: &Value, expected: &Expected, now: u64) -> Result<(Grant, Duration)> {
    let body = value["body"].as_str().context("missing grant")?;
    ensure!(body.len() <= 4096, "grant too large");
    relay_crypto::verify(
        expected.key,
        format!("{PREFIX}{body}").as_bytes(),
        value["signature"]
            .as_str()
            .context("missing grant signature")?,
    )?;
    let grant: Grant = serde_json::from_str(body)?;
    ensure!(grant.v == 1, "unsupported grant version");
    ensure!(
        grant.iss == expected.owner.iss && grant.sub == expected.owner.sub,
        "grant names a different corporate user"
    );
    ensure!(
        grant.machine_id == expected.machine_id,
        "grant names a different computer"
    );
    ensure!(
        grant.context == expected.context && grant.session_id == expected.session_id,
        "grant names a different connection"
    );
    ensure!(
        expected
            .browser_key
            .is_none_or(|key| key == grant.browser_key),
        "grant names a different browser"
    );
    relay_crypto::machine_id(&grant.browser_key)?;
    ensure!(
        grant.exp > grant.iat
            && grant.exp - grant.iat <= MAX_LIFETIME
            && grant.iat <= now + CLOCK_TOLERANCE
            && now < grant.exp + CLOCK_TOLERANCE
            && now <= grant.iat + CLOCK_TOLERANCE,
        "grant expired or not yet valid"
    );
    ensure!(
        (16..=64).contains(&grant.jti.len()),
        "invalid grant identifier"
    );
    let lifetime = Duration::from_secs(grant.exp - grant.iat);
    Ok((grant, lifetime))
}

/// Correlate without recording a usable identifier.
pub fn fingerprint(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// JSON lines. Never pass secrets, tokens, pairing links or terminal content.
pub struct Audit {
    file: Option<(PathBuf, Mutex<()>)>,
}
impl Audit {
    /// The relay is a service: its audit trail goes to stderr for the journal.
    pub fn stderr() -> Self {
        Self { file: None }
    }
    /// The connector's terminal belongs to its dashboard: use a private file.
    pub fn file(path: &Path) -> Self {
        Self {
            file: Some((path.to_owned(), Mutex::new(()))),
        }
    }
    pub fn log(&self, event: &str, mut fields: Value) {
        fields["audit"] = json!("herdr-enterprise");
        fields["event"] = json!(event);
        fields["ts"] = json!(now());
        let line = format!("{fields}\n");
        match &self.file {
            None => {
                // Unit tests exercise denials deliberately; keep their output clean.
                if !cfg!(test) {
                    let _ = std::io::stderr().write_all(line.as_bytes());
                }
            }
            Some((path, lock)) => {
                let _guard = lock.lock().unwrap();
                if std::fs::metadata(path).is_ok_and(|m| m.len() > 4 * 1024 * 1024) {
                    let mut old = path.clone().into_os_string();
                    old.push(".1");
                    let _ = std::fs::rename(path, old);
                }
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(path)
                {
                    let _ = file.write_all(line.as_bytes());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn identity(dir: &Path, name: &str) -> relay_crypto::Identity {
        relay_crypto::Identity::load_or_create(&dir.join(name)).unwrap()
    }
    #[test]
    fn grants_are_bound_to_user_computer_browser_context_session_and_time() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let relay = identity(dir.path(), "relay");
        let browser = identity(dir.path(), "browser").public_key();
        let other_browser = identity(dir.path(), "other").public_key();
        let owner = Principal {
            iss: "https://corp.okta.com".into(),
            sub: "00u1".into(),
        };
        let grant = Grant::new(&owner, "machine", &browser, "https://relay", "session", 300);
        let signed = grant.sign(&relay);
        let key = relay.public_key();
        let expected = || Expected {
            key: &key,
            owner: &owner,
            machine_id: "machine",
            context: "https://relay",
            session_id: "session",
            browser_key: Some(&browser),
        };
        let (verified, lifetime) = verify(&signed, &expected(), now()).unwrap();
        assert_eq!(verified, grant);
        assert_eq!(lifetime, Duration::from_secs(300));
        let other_user = Principal {
            sub: "00u2".into(),
            ..owner.clone()
        };
        let other_issuer = Principal {
            iss: "https://evil.okta.com".into(),
            ..owner.clone()
        };
        let forged_key = identity(dir.path(), "forger").public_key();
        for wrong in [
            Expected {
                owner: &other_user,
                ..expected()
            },
            Expected {
                owner: &other_issuer,
                ..expected()
            },
            Expected {
                machine_id: "another-machine",
                ..expected()
            },
            Expected {
                context: "https://relay/main",
                ..expected()
            },
            Expected {
                session_id: "replayed-into-another-session",
                ..expected()
            },
            Expected {
                browser_key: Some(&other_browser),
                ..expected()
            },
            Expected {
                key: &forged_key,
                ..expected()
            },
        ] {
            assert!(verify(&signed, &wrong, now()).is_err());
        }
        // Expired, replayed-later and future-dated grants are refused.
        assert!(verify(&signed, &expected(), now() + 300 + CLOCK_TOLERANCE).is_err());
        assert!(verify(&signed, &expected(), now() + CLOCK_TOLERANCE + 1).is_err());
        assert!(verify(&signed, &expected(), now() - CLOCK_TOLERANCE - 1).is_err());
        // Malformed or altered envelopes are refused before any field is trusted.
        let mut altered = signed.clone();
        altered["body"] = json!(signed["body"].as_str().unwrap().replace("00u1", "00u2"));
        assert!(
            verify(
                &altered,
                &Expected {
                    owner: &other_user,
                    ..expected()
                },
                now()
            )
            .is_err()
        );
        for malformed in [
            json!(null),
            json!({"body":"{}"}),
            json!({"body":"x","signature":"y"}),
        ] {
            assert!(verify(&malformed, &expected(), now()).is_err());
        }
        let mut long = grant.clone();
        long.exp = long.iat + MAX_LIFETIME + 1;
        assert!(verify(&long.sign(&relay), &expected(), now()).is_err());
    }
    #[test]
    fn audit_file_is_private_json_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let audit = Audit::file(&path);
        audit.log("access-granted", json!({"sub":"00u1"}));
        audit.log("session-end", json!({}));
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "access-granted");
        assert_eq!(lines[0]["sub"], "00u1");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fingerprint("x").len(), 16);
    }
}
