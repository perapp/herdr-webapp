//! Enterprise (Okta) deployment policy: automatic discovery and strict validation.
//!
//! Layers, in precedence order:
//! 1. System policy (`/etc/herdr-webapp/config.toml`, or
//!    `/Library/Application Support/herdr-webapp/config.toml` on macOS).
//! 2. User configuration (`$XDG_CONFIG_HOME/herdr-webapp/config.toml`, default
//!    `~/.config/herdr-webapp/config.toml`), or the explicit `--config` /
//!    `HERDR_WEBAPP_CONFIG` file which replaces only this user layer.
//!
//! An enabled system policy is authoritative: another layer may repeat it exactly
//! but can neither change nor disable it. Anything present but unreadable,
//! malformed, incomplete or conflicting fails startup; there is no fallback to the
//! non-enterprise mode. Same-user enforcement is an invariant, not a setting.
//!
//! This constrains honest installations only. A user who controls their computer
//! can run other software; the relay is the enforcement point and refuses every
//! connector and browser that does not present valid Okta authorization.
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub const DEFAULT_GRANT_LIFETIME: u64 = 300;
pub const DEFAULT_RECHECK: u64 = 300;
pub const DEFAULT_SESSION_MAX: u64 = 12 * 3600;

#[derive(Deserialize)]
struct File {
    enterprise: Option<Section>,
}
#[derive(Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(deny_unknown_fields)]
struct Section {
    enabled: bool,
    relay_url: Option<String>,
    issuer: Option<String>,
    client_id: Option<String>,
    client_secret_file: Option<PathBuf>,
    revocations_file: Option<PathBuf>,
    acr_values: Option<String>,
    #[serde(default)]
    required_amr: Vec<String>,
    grant_lifetime_seconds: Option<u64>,
    recheck_seconds: Option<u64>,
    session_max_seconds: Option<u64>,
}

/// Validated, enabled enterprise policy. Absence means the ordinary deployment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Where the enabling section was found; for diagnostics only.
    pub source: PathBuf,
    /// Canonical public relay origin (scheme and authority only).
    pub relay_url: String,
    pub issuer: String,
    pub client_id: String,
    pub client_secret_file: Option<PathBuf>,
    pub revocations_file: Option<PathBuf>,
    pub acr_values: Option<String>,
    pub required_amr: Vec<String>,
    pub grant_lifetime_seconds: u64,
    pub recheck_seconds: u64,
    pub session_max_seconds: u64,
}

pub fn system_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        "/Library/Application Support/herdr-webapp/config.toml".into()
    } else {
        "/etc/herdr-webapp/config.toml".into()
    }
}
pub fn user_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|base| base.join("herdr-webapp/config.toml"))
}

/// Ordinary startup: no argument is needed. `explicit` replaces the user layer
/// and, unlike a default path, must exist.
pub fn discover(explicit: Option<&Path>, allow_http_loopback: bool) -> Result<Option<Policy>> {
    let user = match explicit {
        Some(path) => Some((path.to_owned(), true)),
        None => user_path().map(|path| (path, false)),
    };
    discover_from(&system_path(), user, allow_http_loopback)
}

pub fn discover_from(
    system: &Path,
    user: Option<(PathBuf, bool)>,
    allow_http_loopback: bool,
) -> Result<Option<Policy>> {
    let system_section = read(system, false, true)?;
    let user_section = match &user {
        Some((path, required)) => read(path, *required, false)?,
        None => None,
    };
    let (section, source) = match (system_section, user_section) {
        (Some(policy), other) if policy.enabled => {
            if let Some(other) = other {
                ensure!(
                    other == policy,
                    "enterprise configuration conflicts with the system policy in {}; \
                     remove the [enterprise] section from the user configuration",
                    system.display()
                );
            }
            (policy, system.to_owned())
        }
        (_, Some(user_policy)) => (user_policy, user.expect("user layer was read").0),
        (Some(policy), None) => (policy, system.to_owned()),
        (None, None) => return Ok(None),
    };
    if !section.enabled {
        return Ok(None);
    }
    validate(section, source, allow_http_loopback).map(Some)
}

fn read(path: &Path, required: bool, system: bool) -> Result<Option<Section>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => {
            return Ok(None);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("read configuration {}", path.display()));
        }
    };
    ensure!(
        metadata.is_file() && metadata.len() <= 65536,
        "configuration {} must be a regular file of at most 64 KiB",
        path.display()
    );
    let uid = unsafe { libc::geteuid() };
    ensure!(
        metadata.mode() & 0o022 == 0
            && (metadata.uid() == 0 || metadata.uid() == uid)
            && (!system || uid != 0 || metadata.uid() == 0),
        "configuration {} must be owned by root or this user and not writable by group or others",
        path.display()
    );
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read configuration {}", path.display()))?;
    let file: File = toml::from_str(&text)
        .with_context(|| format!("malformed configuration {}", path.display()))?;
    Ok(file.enterprise)
}

fn loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Issuers are compared byte-for-byte with the `iss` claim: no normalization.
pub fn valid_issuer(issuer: &str, allow_http_loopback: bool) -> Result<()> {
    ensure!(
        issuer.len() <= 512 && !issuer.chars().any(|c| c.is_control() || c.is_whitespace()),
        "invalid Okta issuer"
    );
    let url = url::Url::parse(issuer).context("invalid Okta issuer")?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && !issuer.ends_with('/'),
        "Okta issuer must be an HTTPS URL without credentials, query, fragment or trailing slash"
    );
    let host = url.host_str().context("Okta issuer host missing")?;
    ensure!(
        url.scheme() == "https"
            || (url.scheme() == "http" && allow_http_loopback && loopback(host)),
        "Okta issuer must use HTTPS"
    );
    Ok(())
}
pub fn valid_client_id(client_id: &str) -> Result<()> {
    ensure!(
        (1..=128).contains(&client_id.len())
            && client_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid Okta client_id"
    );
    Ok(())
}
pub fn valid_lifetimes(grant: u64, recheck: u64, session: u64) -> Result<()> {
    ensure!(
        (30..=900).contains(&grant),
        "grant_lifetime_seconds must be 30..900"
    );
    ensure!(
        (30..=3600).contains(&recheck),
        "recheck_seconds must be 30..3600"
    );
    ensure!(
        (300..=7 * 86400).contains(&session) && session >= recheck,
        "session_max_seconds must be 300..604800 and at least recheck_seconds"
    );
    Ok(())
}

fn validate(section: Section, source: PathBuf, allow_http_loopback: bool) -> Result<Policy> {
    let missing = |name: &str| {
        anyhow::anyhow!(
            "enterprise mode is enabled in {} but `{name}` is missing",
            source.display()
        )
    };
    let relay_url = section
        .relay_url
        .clone()
        .ok_or_else(|| missing("relay_url"))?;
    let issuer = section.issuer.clone().ok_or_else(|| missing("issuer"))?;
    let client_id = section
        .client_id
        .clone()
        .ok_or_else(|| missing("client_id"))?;
    let relay_url = crate::relay_crypto::origin(&relay_url, allow_http_loopback)
        .context("enterprise relay_url must be the relay's public HTTPS origin")?;
    valid_issuer(&issuer, allow_http_loopback)?;
    valid_client_id(&client_id)?;
    for path in [&section.client_secret_file, &section.revocations_file]
        .into_iter()
        .flatten()
    {
        ensure!(
            path.is_absolute(),
            "enterprise file paths must be absolute: {}",
            path.display()
        );
    }
    if let Some(acr) = &section.acr_values {
        ensure!(
            (1..=256).contains(&acr.len())
                && acr.bytes().all(|b| b.is_ascii_graphic() || b == b' '),
            "invalid acr_values"
        );
    }
    ensure!(
        section.required_amr.len() <= 8
            && section
                .required_amr
                .iter()
                .all(|v| (1..=32).contains(&v.len())
                    && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')),
        "invalid required_amr"
    );
    let grant = section
        .grant_lifetime_seconds
        .unwrap_or(DEFAULT_GRANT_LIFETIME);
    let recheck = section.recheck_seconds.unwrap_or(DEFAULT_RECHECK);
    let session = section.session_max_seconds.unwrap_or(DEFAULT_SESSION_MAX);
    valid_lifetimes(grant, recheck, session)?;
    Ok(Policy {
        source,
        relay_url,
        issuer,
        client_id,
        client_secret_file: section.client_secret_file,
        revocations_file: section.revocations_file,
        acr_values: section.acr_values,
        required_amr: section.required_amr,
        grant_lifetime_seconds: grant,
        recheck_seconds: recheck,
        session_max_seconds: session,
    })
}

impl Policy {
    /// A connector without local policy adopts what its relay requires. The relay
    /// is the policy authority and is already the trusted TLS origin.
    pub fn from_relay(
        relay_url: &str,
        advertised: &serde_json::Value,
        allow_http_loopback: bool,
    ) -> Result<Self> {
        let text = |name: &str| {
            advertised[name]
                .as_str()
                .map(str::to_owned)
                .with_context(|| format!("relay enterprise advertisement lacks {name}"))
        };
        let issuer = text("issuer")?;
        let client_id = text("clientId")?;
        valid_issuer(&issuer, allow_http_loopback)?;
        valid_client_id(&client_id)?;
        let number = |name: &str, default: u64| advertised[name].as_u64().unwrap_or(default);
        let grant = number("grantLifetimeSeconds", DEFAULT_GRANT_LIFETIME);
        let recheck = number("recheckSeconds", DEFAULT_RECHECK);
        valid_lifetimes(grant, recheck, DEFAULT_SESSION_MAX.max(recheck))?;
        Ok(Self {
            source: PathBuf::from(format!("{relay_url}/api/mode")),
            relay_url: relay_url.to_owned(),
            issuer,
            client_id,
            client_secret_file: None,
            revocations_file: None,
            acr_values: advertised["acrValues"].as_str().map(str::to_owned),
            required_amr: serde_json::from_value(advertised["requiredAmr"].clone())
                .unwrap_or_default(),
            grant_lifetime_seconds: grant,
            recheck_seconds: recheck,
            session_max_seconds: DEFAULT_SESSION_MAX.max(recheck),
        })
    }
    /// Local policy pins the identity provider; a relay may not substitute another.
    pub fn ensure_matches_relay(&self, advertised: &serde_json::Value) -> Result<()> {
        if advertised["required"] != true
            || advertised["issuer"] != self.issuer.as_str()
            || advertised["clientId"] != self.client_id.as_str()
        {
            bail!(
                "enterprise mode is required by {} but the relay does not enforce the same Okta \
                 issuer and client; refusing to connect without it",
                self.source.display()
            );
        }
        Ok(())
    }
    pub fn advertisement(&self) -> serde_json::Value {
        serde_json::json!({
            "required": true,
            "issuer": self.issuer,
            "clientId": self.client_id,
            "acrValues": self.acr_values,
            "requiredAmr": self.required_amr,
            "grantLifetimeSeconds": self.grant_lifetime_seconds,
            "recheckSeconds": self.recheck_seconds,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    const ENABLED: &str = "[enterprise]\nenabled = true\nrelay_url = \"https://relay.example\"\nissuer = \"https://corp.okta.com\"\nclient_id = \"0oaabc\"\n";
    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        path
    }
    #[test]
    fn default_paths_are_discovered_without_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.toml");
        assert!(
            discover_from(&missing, Some((missing.clone(), false)), false)
                .unwrap()
                .is_none()
        );
        let user = write(dir.path(), "user.toml", ENABLED);
        let policy = discover_from(&missing, Some((user.clone(), false)), false)
            .unwrap()
            .unwrap();
        assert_eq!(policy.source, user);
        assert_eq!(policy.relay_url, "https://relay.example");
        assert_eq!(policy.grant_lifetime_seconds, DEFAULT_GRANT_LIFETIME);
        let system = write(dir.path(), "system.toml", ENABLED);
        let policy = discover_from(&system, None, false).unwrap().unwrap();
        assert_eq!(policy.source, system);
        assert!(user_path().is_some());
        assert!(system_path().is_absolute());
    }
    #[test]
    fn system_policy_cannot_be_changed_or_disabled_by_user_or_explicit_layers() {
        let dir = tempfile::tempdir().unwrap();
        let system = write(dir.path(), "system.toml", ENABLED);
        let same = write(dir.path(), "same.toml", ENABLED);
        assert!(discover_from(&system, Some((same, true)), false).is_ok());
        for other in [
            "[enterprise]\nenabled = false\n".to_owned(),
            ENABLED.replace("corp.okta.com", "evil.okta.com"),
            ENABLED.replace("relay.example", "other.example"),
            format!("{ENABLED}grant_lifetime_seconds = 900\n"),
        ] {
            for explicit in [false, true] {
                let user = write(dir.path(), "user.toml", &other);
                assert!(discover_from(&system, Some((user, explicit)), false).is_err());
            }
        }
        // A disabled or absent system section leaves the choice to the user layer.
        let system = write(dir.path(), "system.toml", "[enterprise]\nenabled = false\n");
        let user = write(dir.path(), "user.toml", ENABLED);
        assert!(
            discover_from(&system, Some((user, false)), false)
                .unwrap()
                .is_some()
        );
        assert!(discover_from(&system, None, false).unwrap().is_none());
    }
    #[test]
    fn explicit_override_must_exist_and_replaces_only_the_user_layer() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.toml");
        assert!(discover_from(&missing, Some((missing.clone(), true)), false).is_err());
        let explicit = write(dir.path(), "explicit.toml", ENABLED);
        assert!(
            discover_from(&missing, Some((explicit, true)), false)
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn invalid_incomplete_or_unsafe_configuration_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.toml");
        for text in [
            "[enterprise".to_owned(),
            "[enterprise]\nenabled = \"yes\"\n".to_owned(),
            "[enterprise]\nenabled = true\n".to_owned(),
            format!("{ENABLED}require_same_user = false\n"),
            ENABLED.replace("https://corp.okta.com", "http://corp.okta.com"),
            ENABLED.replace("https://corp.okta.com", "https://corp.okta.com/"),
            ENABLED.replace("https://relay.example", "https://relay.example/main"),
            ENABLED.replace("https://relay.example", "http://relay.example"),
            ENABLED.replace("0oaabc", "bad id"),
            format!("{ENABLED}grant_lifetime_seconds = 5\n"),
            format!("{ENABLED}grant_lifetime_seconds = 86400\n"),
            format!("{ENABLED}recheck_seconds = 1\n"),
            format!("{ENABLED}client_secret_file = \"relative\"\n"),
            format!("{ENABLED}required_amr = [\"bad value\"]\n"),
        ] {
            let user = write(dir.path(), "user.toml", &text);
            assert!(
                discover_from(&missing, Some((user, false)), false).is_err(),
                "{text}"
            );
        }
        let user = write(dir.path(), "user.toml", ENABLED);
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(discover_from(&missing, Some((user.clone(), false)), false).is_err());
        assert!(discover_from(&user, None, false).is_err());
        // Loopback HTTP is development-only and needs the explicit flag.
        let dev = ENABLED
            .replace("https://corp.okta.com", "http://127.0.0.1:9000")
            .replace("https://relay.example", "http://127.0.0.1:9001");
        let user = write(dir.path(), "user.toml", &dev);
        assert!(discover_from(&missing, Some((user.clone(), false)), false).is_err());
        assert!(discover_from(&missing, Some((user, false)), true).is_ok());
    }
    #[test]
    fn relay_advertisement_is_validated_and_local_policy_pins_the_provider() {
        let dir = tempfile::tempdir().unwrap();
        let user = write(dir.path(), "user.toml", ENABLED);
        let policy = discover_from(&dir.path().join("none"), Some((user, false)), false)
            .unwrap()
            .unwrap();
        let advertised = policy.advertisement();
        policy.ensure_matches_relay(&advertised).unwrap();
        let adopted = Policy::from_relay("https://relay.example", &advertised, false).unwrap();
        assert_eq!(adopted.issuer, policy.issuer);
        assert_eq!(adopted.client_id, policy.client_id);
        for (key, value) in [
            ("issuer", serde_json::json!("https://evil.okta.com")),
            ("clientId", serde_json::json!("other")),
            ("required", serde_json::json!(false)),
        ] {
            let mut changed = advertised.clone();
            changed[key] = value;
            assert!(policy.ensure_matches_relay(&changed).is_err());
        }
        assert!(
            policy
                .ensure_matches_relay(&serde_json::Value::Null)
                .is_err()
        );
        let mut bad = advertised.clone();
        bad["issuer"] = serde_json::json!("http://corp.okta.com");
        assert!(Policy::from_relay("https://relay.example", &bad, false).is_err());
        bad = advertised;
        bad["grantLifetimeSeconds"] = serde_json::json!(100000);
        assert!(Policy::from_relay("https://relay.example", &bad, false).is_err());
    }
}
