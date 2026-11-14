use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
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
    #[command(subcommand)]
    pub mode: Option<Mode>,
    /// Display the project license and locally vendored dependency notices.
    #[arg(long, global = true)]
    pub licenses: bool,
    /// Report compiled source provenance (used by verified main-channel builds).
    #[arg(long, global = true, hide = true)]
    pub build_revision: bool,
    /// Internal verified-candidate helper; never accepted from the HTTP API.
    #[arg(long, hide = true)]
    pub update_helper: Option<PathBuf>,
    #[arg(long, global = true, env = "HERDR_WEB_BIND", default_value = "0.0.0.0")]
    pub bind: IpAddr,
    /// Explicit port: fail if occupied. Otherwise select the first free port from 8001.
    #[arg(long, global = true, env = "HERDR_WEB_PORT")]
    pub port: Option<u16>,
    #[arg(long, global = true, env = "HC_SOCKET")]
    pub core_socket: Option<PathBuf>,
    #[arg(long, global = true, env = "HC_SESSION")]
    pub session: Option<String>,
    #[arg(long, global = true, env = "HERDR_WEB_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,
    /// Exact external origin when behind an HTTPS reverse proxy (e.g. https://herdr.example).
    #[arg(long, global = true, env = "HERDR_WEB_PUBLIC_ORIGIN")]
    pub public_origin: Option<String>,
    /// Plain output instead of the pinned QR / URL dashboard (automatic when redirected).
    #[arg(long, global = true)]
    pub no_dashboard: bool,
}

#[derive(Debug, Subcommand)]
pub enum Mode {
    /// Direct LAN access (also the default without a subcommand).
    Local,
    /// Host the browser application, pairing/signaling and approved client downloads.
    ServeRelay(ServeRelay),
    /// Connect local Herdr Core outbound to a hosted relay and display pairing QR.
    ConnectRelay(ConnectRelay),
}
#[derive(Debug, Args)]
pub struct ServeRelay {
    /// Verified public client catalog, prepared with scripts/prepare-relay-release.py.
    #[arg(long, env = "HERDR_RELAY_RELEASE_DIR")]
    pub release_dir: Option<PathBuf>,
    /// Explicitly opt in to fetching/building trusted main source locally.
    #[arg(long)]
    pub enable_main: bool,
    /// Poll interval; builds run only for changed revisions (e.g. 5m, 30s).
    #[arg(long, default_value = "5m", value_parser = main_interval)]
    pub main_update_interval: u64,
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u16).range(1..=16))]
    pub main_build_jobs: u16,
    /// Trusted Git repository. Never sourced from a web request.
    #[arg(
        long,
        env = "HERDR_MAIN_REPOSITORY",
        default_value = "https://github.com/perapp/herdr-webapp.git"
    )]
    pub main_repository: String,
    #[arg(long, default_value = "main")]
    pub main_branch: String,
    /// Dedicated owner-only persistent checkout, build cache and artifact directory.
    #[arg(long, env = "HERDR_MAIN_STATE_DIR")]
    pub main_state_dir: Option<PathBuf>,
    /// Internal child only; the public Origin remains scheme/authority, without a path.
    #[arg(long, hide = true, default_value = "/")]
    pub channel_base_path: String,
    #[arg(long, hide = true)]
    pub channel_revision: Option<String>,
    /// Internal backend; never binds a public TCP listener.
    #[arg(long, hide = true)]
    pub unix_socket: Option<PathBuf>,
    /// TURN REST shared-secret file, owner-only; never pass the secret in argv.
    #[arg(long, env = "HERDR_RELAY_TURN_SECRET_FILE")]
    pub turn_secret_file: Option<PathBuf>,
    /// Coturn URL (repeat for UDP/TCP/TLS); not an HTTP reverse-proxy address.
    #[arg(
        long = "turn-url",
        env = "HERDR_RELAY_TURN_URLS",
        value_delimiter = ','
    )]
    pub turn_urls: Vec<String>,
    #[arg(long, default_value_t = 600)]
    pub turn_ttl_seconds: u64,
    /// Development only: permit HTTP with a loopback origin and bind address.
    #[arg(long)]
    pub allow_http_loopback: bool,
}
#[derive(Debug, Args)]
pub struct ConnectRelay {
    #[arg(long, env = "HERDR_RELAY_URL")]
    pub relay_url: String,
    /// Persistent private laptop identity and paired-browser keys.
    #[arg(long, env = "HERDR_RELAY_STATE_DIR")]
    pub state_dir: Option<PathBuf>,
    /// Private supervisor readiness marker, never a pairing credential.
    #[arg(long)]
    pub ready_file: Option<PathBuf>,
    #[arg(long)]
    pub allow_http_loopback: bool,
    /// Require TURN relay candidates, useful for network verification.
    #[arg(long)]
    pub force_turn: bool,
    /// Explicitly revoke all stored browser pairings before connecting.
    #[arg(long)]
    pub reset_pairings: bool,
}

fn main_interval(value: &str) -> std::result::Result<u64, String> {
    let (number, scale) = match value.as_bytes().last() {
        Some(b's') => (&value[..value.len() - 1], 1),
        Some(b'm') => (&value[..value.len() - 1], 60),
        Some(b'h') => (&value[..value.len() - 1], 3600),
        _ => (value, 1),
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .filter(|n| (1..=86400).contains(n))
        .ok_or_else(|| "main interval must be 1s..24h".into())
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
    fn modes_keep_legacy_flags_and_offline_license_output() {
        let legacy = Config::try_parse_from(["app", "--port", "8123", "--no-dashboard"]).unwrap();
        assert!(legacy.mode.is_none());
        assert_eq!(legacy.port, Some(8123));
        let local = Config::try_parse_from(["app", "local", "--port", "8123"]).unwrap();
        assert!(matches!(local.mode, Some(Mode::Local)));
        assert_eq!(local.port, legacy.port);
        let notices = Config::try_parse_from(["app", "--licenses"]).unwrap();
        assert!(notices.licenses && notices.mode.is_none());
        let connector = Config::try_parse_from([
            "app",
            "connect-relay",
            "--relay-url",
            "https://relay.example",
            "--force-turn",
        ])
        .unwrap();
        assert!(matches!(connector.mode, Some(Mode::ConnectRelay(options)) if options.force_turn));
    }
    #[test]
    fn main_channel_is_opt_in_with_bounded_controls() {
        let normal = Config::try_parse_from(["app", "serve-relay"]).unwrap();
        assert!(
            matches!(normal.mode, Some(Mode::ServeRelay(o)) if !o.enable_main && o.main_update_interval == 300 && o.main_build_jobs == 2 && o.unix_socket.is_none())
        );
        let opted = Config::try_parse_from([
            "app",
            "serve-relay",
            "--enable-main",
            "--main-update-interval",
            "2m",
            "--main-build-jobs",
            "1",
        ])
        .unwrap();
        assert!(
            matches!(opted.mode, Some(Mode::ServeRelay(o)) if o.enable_main && o.main_update_interval == 120 && o.main_build_jobs == 1)
        );
        for (key, value) in [
            ("--main-update-interval", "0s"),
            ("--main-update-interval", "25h"),
            ("--main-update-interval", "bad"),
            ("--main-build-jobs", "0"),
            ("--main-build-jobs", "17"),
        ] {
            assert!(Config::try_parse_from(["app", "serve-relay", key, value]).is_err());
        }
    }
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
