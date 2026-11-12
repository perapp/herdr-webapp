use anyhow::{Context, Result, bail};
use clap::Parser;
use rand::RngCore;
use std::{
    net::IpAddr,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Herdr in your browser. Authenticated users have full shell access."
)]
pub struct Config {
    #[arg(long, env = "HERDR_WEB_BIND", default_value = "0.0.0.0")]
    pub bind: IpAddr,
    /// Explicit port: fail if occupied. Otherwise select the first free port from 8001.
    #[arg(long, env = "HERDR_WEB_PORT")]
    pub port: Option<u16>,
    #[arg(long, env = "HC_SOCKET")]
    pub core_socket: Option<PathBuf>,
    #[arg(long, env = "HC_SESSION")]
    pub session: Option<String>,
    #[arg(long, env = "HERDR_WEB_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,
    /// Exact external origin when behind an HTTPS reverse proxy (e.g. https://herdr.example).
    #[arg(long, env = "HERDR_WEB_PUBLIC_ORIGIN")]
    pub public_origin: Option<String>,
    /// Plain output instead of the pinned QR / URL dashboard (automatic when redirected).
    #[arg(long)]
    pub no_dashboard: bool,
}

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("HOME is required")?,
    ))
}

impl Config {
    pub fn socket(&self) -> Result<PathBuf> {
        if let Some(path) = &self.core_socket {
            return Ok(path.clone());
        }
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or(home()?.join(".config"));
        let path = config_home.join("herdr-core/config.toml");
        if path.exists() {
            let config: toml::Value = toml::from_str(&std::fs::read_to_string(&path)?)
                .with_context(|| format!("parse {}", path.display()))?;
            if let Some(s) = config.get("socket").and_then(|s| s.as_str()) {
                return Ok(s.into());
            }
        }
        let session = self
            .session
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("default");
        if session == "."
            || session == ".."
            || session
                .chars()
                .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        {
            bail!("invalid core session name");
        }
        let run = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                let path = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
                path.is_dir().then_some(path)
            });
        let base = if let Some(run) = run {
            run.join("herdr-core")
        } else {
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or(home()?.join(".local/state"))
                .join("herdr-core")
                .join(session)
        };
        Ok(base.join(format!("{session}.sock")))
    }

    pub fn token_path(&self) -> Result<PathBuf> {
        Ok(self.token_file.clone().unwrap_or(
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or(home()?.join(".local/state"))
                .join("herdr-webapp/auth-token"),
        ))
    }

    /// A sidecar follows the configured token, including custom token locations.
    pub fn session_path(&self) -> Result<PathBuf> {
        let mut path = self.token_path()?.into_os_string();
        path.push(".sessions");
        Ok(path.into())
    }

    pub fn token(&self) -> Result<String> {
        let path = self.token_path()?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        let mut random = [0u8; 32];
        rand::rng().fill_bytes(&mut random);
        let generated = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(generated.as_bytes())?;
                file.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).context("create private authentication token"),
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != unsafe { libc::getuid() }
        {
            bail!(
                "token file must be a regular, user-owned file with mode 0600: {}",
                path.display()
            );
        }
        use std::io::Read;
        let mut token = String::new();
        file.take(4097).read_to_string(&mut token)?;
        let token = token.trim();
        if token.len() < 32
            || token.len() > 4096
            || !token
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            bail!("token file must contain 32–4096 URL-safe characters");
        }
        Ok(token.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_is_private_and_reused() {
        let d = tempfile::tempdir().unwrap();
        let c = Config::parse_from([
            "app",
            "--token-file",
            d.path().join("token").to_str().unwrap(),
        ]);
        let t = c.token().unwrap();
        assert_eq!(t.len(), 64);
        assert_eq!(c.token().unwrap(), t);
        std::fs::set_permissions(
            c.token_file.as_ref().unwrap(),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(c.token().is_err());
    }
    #[test]
    fn token_symlink_is_refused() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("real"), "a".repeat(64)).unwrap();
        std::os::unix::fs::symlink(d.path().join("real"), d.path().join("link")).unwrap();
        let c = Config::parse_from([
            "app",
            "--token-file",
            d.path().join("link").to_str().unwrap(),
        ]);
        assert!(c.token().is_err());
    }
}
