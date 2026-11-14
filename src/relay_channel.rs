//! Strict channel endpoints. Channels are URL context, never HTTP Origins.
use anyhow::{Context, Result, ensure};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayEndpoint {
    pub origin: String,
    pub base_path: String,
}
impl RelayEndpoint {
    pub fn parse(value: &str, allow_http: bool) -> Result<Self> {
        // Inspect raw path before URL parsing can normalize dot segments or escapes.
        let (_, rest) = value.split_once("://").context("Invalid relay URL")?;
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let suffix = &rest[authority_end..];
        ensure!(
            matches!(suffix, "" | "/" | "/main" | "/main/"),
            "Relay URL must be an origin or /main endpoint"
        );
        let authority = &value[..value.len() - suffix.len()];
        let origin = crate::relay_crypto::origin(authority, allow_http)?;
        Ok(Self {
            origin,
            base_path: if suffix.starts_with("/main") {
                "/main/"
            } else {
                "/"
            }
            .into(),
        })
    }
    /// Called under the connector lock, before loading identity/pairings. Explicit
    /// --state-dir overrides must not silently merge two channel identities.
    pub fn bind_state(&self, directory: &std::path::Path) -> Result<()> {
        let path = directory.join("endpoint.json");
        if path.try_exists()? {
            let bytes = crate::relay_crypto::read_private(&path, 4096)?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            ensure!(
                value["schemaVersion"] == 1 && value["relayUrl"] == self.url(),
                "Connector state belongs to another relay/channel; use a separate state directory"
            );
        } else {
            ensure!(
                self.base_path == "/" || !directory.join("identity.json").try_exists()?,
                "Legacy connector state cannot be adopted by main; use a separate state directory"
            );
            crate::relay_crypto::write_private(
                &path,
                &serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"relayUrl":self.url()}))?,
            )?;
        }
        Ok(())
    }
    pub fn url(&self) -> String {
        if self.base_path == "/" {
            self.origin.clone()
        } else {
            format!("{}/main", self.origin)
        }
    }
    pub fn root_url(&self) -> String {
        format!("{}{}", self.origin, self.base_path)
    }
    pub fn path(&self, path: &str) -> String {
        if self.base_path == "/" {
            path.into()
        } else {
            format!("/main{path}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_state_overrides_cannot_merge_channels_and_legacy_root_is_preserved() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(dir.path().join("identity.json"), b"legacy").unwrap();
        let root = RelayEndpoint::parse("https://relay.example", false).unwrap();
        let main = RelayEndpoint::parse("https://relay.example/main", false).unwrap();
        assert!(main.bind_state(dir.path()).is_err());
        root.bind_state(dir.path()).unwrap();
        root.bind_state(dir.path()).unwrap();
        assert!(main.bind_state(dir.path()).is_err());
    }
    #[test]
    fn channel_context_is_not_origin() {
        for input in ["https://relay.example", "https://relay.example/"] {
            let e = RelayEndpoint::parse(input, false).unwrap();
            assert_eq!(e.url(), "https://relay.example");
            assert_eq!(e.root_url(), "https://relay.example/");
            assert_eq!(e.path("/relay/socket"), "/relay/socket");
        }
        for input in ["https://relay.example/main", "https://relay.example/main/"] {
            let e = RelayEndpoint::parse(input, false).unwrap();
            assert_eq!(e.origin, "https://relay.example");
            assert_eq!(e.url(), "https://relay.example/main");
            assert_eq!(e.root_url(), "https://relay.example/main/");
            assert_eq!(e.path("/relay/socket"), "/main/relay/socket");
        }
    }
    #[test]
    fn rejects_normalized_or_ambiguous_paths_and_noncanonical_authorities() {
        for input in [
            "https://relay/main/../",
            "https://relay/%6dain",
            "https://relay//main",
            "https://relay/main?",
            "https://relay/main#",
            "https://relay/other",
            "https://user@relay/main",
            "https://RELAY/main",
            "https://relay:443/main",
            "https://relay\\evil/main",
            "http://relay/main",
        ] {
            assert!(RelayEndpoint::parse(input, true).is_err(), "{input}");
        }
        assert!(RelayEndpoint::parse("http://127.0.0.1:9000/main", true).is_ok());
    }
}
