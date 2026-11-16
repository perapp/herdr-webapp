//! Private-release updater. No caller-controlled repository, URL, target or filesystem path.
use crate::{
    Shared,
    auth::{ApiError, error},
};
use anyhow::{Result, bail, ensure};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    convert::Infallible,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Notify, watch};
const REPO: &str = "perapp/herdr-webapp";
const VERSION: &str = crate::build_info::RELEASE_VERSION;
const FILES: [&str; 3] = [
    "herdr-webapp",
    "pi-extension/commands.ts",
    "scripts/install-pi-extension.sh",
];
/// Recognize only published release identities, never a compatible-looking host.
/// In particular `target_arch = "arm"` also includes unpublished ARMv6 builds,
/// and Linux GNU and musl receipts/artifacts must never be interchangeable.
fn release_target(build_target: &str) -> &'static str {
    match build_target {
        "x86_64-unknown-linux-gnu" => "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-unknown-linux-gnu",
        "armv7-unknown-linux-gnueabihf" => "armv7-unknown-linux-gnueabihf",
        "i686-unknown-linux-gnu" => "i686-unknown-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl" => "aarch64-unknown-linux-musl",
        "aarch64-apple-darwin" => "aarch64-apple-darwin",
        "x86_64-apple-darwin" => "x86_64-apple-darwin",
        _ => "unsupported",
    }
}
fn target() -> &'static str {
    // Set by build.rs from Cargo's TARGET, not the machine executing the binary
    // or an inherited HERDR_BUILD_TARGET override.
    release_target(env!("HERDR_BUILD_TARGET"))
}
#[derive(Clone, Debug, Serialize)]
pub struct UpdateStatus {
    current_version: String,
    available_version: Option<String>,
    phase: String,
    message: String,
    can_install: bool,
    release_url: Option<String>,
    checked_at: Option<u64>,
    source: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Receipt {
    schema_version: u32,
    repository: String,
    target: String,
    version: String,
}
fn receipt(version: &str) -> Receipt {
    Receipt {
        schema_version: 1,
        repository: REPO.into(),
        target: target().into(),
        version: version.into(),
    }
}
fn private(path: &Path, directory: bool) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.uid() == unsafe { libc::getuid() }
            && m.mode() & 0o077 == 0
            && !m.file_type().is_symlink(),
        "Unsafe update permissions"
    );
    ensure!(
        if directory {
            m.is_dir() && m.mode() & 0o200 != 0
        } else {
            m.is_file() && m.nlink() == 1
        },
        "Unsafe update file"
    );
    Ok(())
}
fn safe_parent(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir()
            && m.uid() == unsafe { libc::getuid() }
            && m.mode() & 0o022 == 0
            && m.mode() & 0o200 != 0
            && fs::canonicalize(path)? == path,
        "Unsafe installation parent"
    );
    Ok(())
}
fn read_private(path: &Path, max: usize) -> Result<Vec<u8>> {
    private(path, false)?;
    let mut out = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut out)?;
    ensure!(out.len() <= max, "Update file exceeds limit");
    Ok(out)
}
fn write_private(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
fn eligible(exe: &Path) -> Result<PathBuf> {
    eligible_for_version(exe, VERSION)
}
fn eligible_for_version(exe: &Path, version: &str) -> Result<PathBuf> {
    ensure!(
        exe.file_name().is_some_and(|n| n == "herdr-webapp") && fs::canonicalize(exe)? == exe,
        "Not a managed installation"
    );
    let root = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid installation"))?;
    private(root, true)?;
    safe_parent(root.parent().unwrap())?;
    let m = fs::symlink_metadata(exe)?;
    ensure!(
        m.is_file()
            && !m.file_type().is_symlink()
            && m.uid() == unsafe { libc::getuid() }
            && m.mode() & 0o022 == 0,
        "Unsafe executable"
    );
    let r: Receipt = serde_json::from_slice(&read_private(&root.join("install.json"), 4096)?)?;
    ensure!(
        r.schema_version == 1
            && r.repository == REPO
            && r.target == target()
            && r.version == version
            && semver(version).is_ok()
            && target() != "unsupported",
        "Installation receipt does not match this executable"
    );
    Ok(root.into())
}
fn sidecar(root: &Path, suffix: &str) -> PathBuf {
    let mut p = root.as_os_str().to_os_string();
    p.push(suffix);
    p.into()
}
struct Pid {
    path: PathBuf,
    bytes: Vec<u8>,
}
impl Drop for Pid {
    fn drop(&mut self) {
        if read_private(&self.path, 64).ok().as_ref() == Some(&self.bytes) {
            let _ = fs::remove_file(&self.path);
        }
    }
}
fn register_pid(root: &Path) -> Result<Pid> {
    let path = sidecar(root, ".pid");
    if path.exists() {
        let bytes = read_private(&path, 64)?;
        let pid = std::str::from_utf8(&bytes)?.trim().parse::<i32>()?;
        if pid == std::process::id() as i32 {
            return Ok(Pid { path, bytes });
        }
        ensure!(
            pid > 0
                && unsafe { libc::kill(pid, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
            "Managed server is already running"
        );
        fs::remove_file(&path)?;
    }
    let bytes = format!("{}\n", std::process::id()).into_bytes();
    write_private(&path, &bytes, 0o600)?;
    Ok(Pid { path, bytes })
}
/// Strict stable semver; build metadata is intentionally not accepted in release tags.
fn semver(s: &str) -> Result<(u64, u64, u64)> {
    let p: Vec<_> = s.split('.').collect();
    ensure!(p.len() == 3, "Invalid release version");
    let mut n = [0; 3];
    for (i, v) in p.iter().enumerate() {
        ensure!(
            !v.is_empty()
                && (v.len() == 1 || !v.starts_with('0'))
                && v.bytes().all(|b| b.is_ascii_digit()),
            "Invalid release version"
        );
        n[i] = v.parse()?;
    }
    Ok((n[0], n[1], n[2]))
}
/// Bound stdout while reading; never collect or forward gh stderr (may contain signed URLs).
fn output(mut command: Command, max: usize) -> Result<Vec<u8>> {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("Required updater command unavailable"))?;
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut data = Vec::new();
        let result = stdout.take(max as u64 + 1).read_to_end(&mut data);
        let _ = tx.send((result, data));
    });
    let result = rx.recv_timeout(Duration::from_secs(120));
    match result {
        Ok((Ok(_), data)) if data.len() <= max => {
            if child.wait()?.success() {
                Ok(data)
            } else {
                bail!(
                    "GitHub request or archive verification failed; check gh auth status and repository access"
                )
            }
        }
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Update command exceeded its time or size limit")
        }
    }
}
fn gh(endpoint: &str, binary: bool, max: usize) -> Result<Vec<u8>> {
    let mut c = Command::new("gh");
    c.args(["api", "--hostname", "github.com", endpoint]);
    if binary {
        c.args(["-H", "Accept: application/octet-stream"]);
    }
    c.env("GH_PROMPT_DISABLED", "1");
    output(c, max)
}
#[derive(Deserialize)]
struct Asset {
    id: u64,
    name: String,
}
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
fn latest() -> Result<(String, Release)> {
    let r: Release = serde_json::from_slice(&gh(
        &format!("repos/{REPO}/releases/latest"),
        false,
        128 * 1024,
    )?)?;
    ensure!(!r.draft && !r.prerelease, "No stable release is available");
    let v = r
        .tag_name
        .strip_prefix('v')
        .ok_or_else(|| anyhow::anyhow!("Invalid release tag"))?
        .to_owned();
    semver(&v)?;
    Ok((v, r))
}
fn checksum(text: &[u8], name: &str) -> Result<String> {
    let mut found = None;
    for line in std::str::from_utf8(text)?.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(
            fields.len() == 2
                && fields[0].len() == 64
                && fields[0].bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid SHA256SUMS"
        );
        if fields[1].trim_start_matches('*') == name {
            ensure!(found.is_none(), "Duplicate checksum");
            found = Some(fields[0].to_ascii_lowercase());
        }
    }
    found.ok_or_else(|| anyhow::anyhow!("Release checksum missing"))
}
fn archive_name(name: &str) -> bool {
    FILES.contains(&name) || ["LICENSE", "pi-extension/", "scripts/"].contains(&name)
}
fn tar(archive: &Path, flag: &str, name: Option<&str>, max: usize) -> Result<Vec<u8>> {
    let mut c = Command::new("tar");
    c.arg(flag).arg(archive);
    if let Some(n) = name {
        c.arg("--").arg(n);
    }
    output(c, max)
}
struct Lock {
    path: PathBuf,
    held: bool,
}
impl Lock {
    fn acquire(root: &Path) -> Result<Self> {
        let path = sidecar(root, ".update-lock");
        fs::DirBuilder::new().mode(0o700).create(&path).map_err(|_|anyhow::anyhow!("Update lock exists. If an update crashed, stop this web server and verify no installer/helper is running before manually removing the sibling .update-lock directory."))?;
        Ok(Self { path, held: true })
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        if self.held {
            let _ = fs::remove_dir(&self.path);
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    root: PathBuf,
    stage: PathBuf,
    version: String,
    args: Vec<String>,
    identity: RestartIdentity,
}
/// exec preserves this identity, including the foreground job the shell is waiting for.
/// No tcsetpgrp, session creation, stdin substitution, or signal-disposition changes.
#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RestartIdentity {
    pid: u32,
    session: i32,
    process_group: i32,
}
impl RestartIdentity {
    fn capture() -> Self {
        Self {
            pid: std::process::id(),
            session: unsafe { libc::getsid(0) },
            process_group: unsafe { libc::getpgrp() },
        }
    }
    fn validate(&self) -> Result<()> {
        let current = Self::capture();
        ensure!(
            self.pid == current.pid
                && self.session > 0
                && self.session == current.session
                && self.process_group > 0
                && self.process_group == current.process_group,
            "Restart must exec in the original server process and terminal job"
        );
        Ok(())
    }
}
pub struct Updater {
    status: watch::Sender<UpdateStatus>,
    busy: Mutex<bool>,
    root: Option<PathBuf>,
    args: Mutex<Vec<String>>,
    pending_helper: Mutex<Option<PathBuf>>,
    pub shutdown: Notify,
    _pid: Option<Pid>,
}
impl Updater {
    pub fn development() -> Arc<Self> {
        Self::new_inner(None, None)
    }
    fn new_inner(root: Option<PathBuf>, pid: Option<Pid>) -> Arc<Self> {
        let managed = root.is_some();
        let (status,_)=watch::channel(UpdateStatus{current_version:crate::build_info::VERSION.into(),available_version:None,phase:"idle".into(),message:if managed {"Ready to check for updates"} else {"Development/source build: updates can be checked, but installation is disabled. Use the release installer."}.into(),can_install:managed,release_url:None,checked_at:None,source:if managed{"release"}else{"development"}.into()});
        Arc::new(Self {
            status,
            busy: Mutex::new(false),
            root,
            args: Mutex::new(vec![]),
            pending_helper: Mutex::new(None),
            shutdown: Notify::new(),
            _pid: pid,
        })
    }
    pub fn startup() -> Result<Arc<Self>> {
        let root = std::env::current_exe().ok().and_then(|p| eligible(&p).ok());
        let pid = root
            .as_ref()
            .map(|r| {
                // Serialize startup's PID registration with an external installer.
                let _lock = Lock::acquire(r)?;
                register_pid(r)
            })
            .transpose()?;
        Ok(Self::new_inner(root, pid))
    }
    /// Retire only a validated older managed backup, after this server has bound HTTP.
    /// Until this point the helper's rename/exec rollback retains the complete backup.
    pub fn run_ready(&self) {
        if let Some(root) = &self.root {
            let result = (|| -> Result<()> {
                let _lock = Lock::acquire(root)?;
                eligible(&root.join("herdr-webapp"))?;
                retire_backup(root)
            })();
            if result.is_err() {
                self.phase("error", "The server is ready, but its previous backup could not be safely retired. No backup was removed; inspect the managed backup and update lock before the next update.");
            }
        }
    }
    /// Called after the bounded HTTP drain. Unix exec replaces all runtime threads,
    /// closes CLOEXEC sockets/auth locks, and retains PID, TTY, stdio, cwd and env.
    /// In particular the invoking shell never observes its foreground job exiting.
    pub fn exec_pending(&self) -> Result<()> {
        if let Some(path) = self.pending_helper.lock().unwrap().take() {
            let error = Command::new(path.parent().unwrap().join("herdr-webapp"))
                .arg("--update-helper")
                .arg(path)
                .exec();
            return Err(anyhow::anyhow!(
                "Unable to exec verified update helper: {}",
                error.kind()
            ));
        }
        Ok(())
    }
    pub fn configure(&self, args: Vec<String>) {
        *self.args.lock().unwrap() = args;
    }
    fn phase(&self, phase: &str, message: &str) {
        self.status.send_modify(|s| {
            s.phase = phase.into();
            s.message = message.into();
        });
    }
    fn begin(&self) -> Result<()> {
        let mut busy = self.busy.lock().unwrap();
        ensure!(!*busy, "An update operation is already running");
        *busy = true;
        Ok(())
    }
    pub fn automatic(self: Arc<Self>) {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            loop {
                let mut changes = self.status.subscribe();
                if self.begin().is_ok() {
                    self.clone().launch(None);
                }
                loop {
                    let running = matches!(
                        changes.borrow_and_update().phase.as_str(),
                        "checking" | "downloading"
                    );
                    if !running || changes.changed().await.is_err() {
                        break;
                    }
                }
                let delay = if self.status.borrow().phase == "error" {
                    1800
                } else {
                    3600
                };
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
        });
    }
    fn launch(self: Arc<Self>, install: Option<String>) {
        self.phase(
            if install.is_some() {
                "downloading"
            } else {
                "checking"
            },
            if install.is_some() {
                "Downloading and verifying release"
            } else {
                "Checking GitHub for a stable release"
            },
        );
        tokio::task::spawn_blocking(move || {
            let result = self.operation(install);
            if let Err(e) = result {
                let message = if e.to_string().starts_with("Update lock exists")
                    || e.to_string().starts_with("Previous update backup exists")
                {
                    e.to_string()
                } else {
                    "Update failed. Ensure gh is installed and run gh auth status; verify access to the private repository. No installation was activated. If a helper crashed, inspect the sibling update lock before recovery.".into()
                };
                self.phase("error", &message);
            }
            *self.busy.lock().unwrap() = false;
        });
    }
    fn operation(&self, install: Option<String>) -> Result<()> {
        let (version, release) = latest()?;
        let newer = match semver(VERSION) {
            Ok(current) => semver(&version)? > current,
            Err(_) => true,
        };
        self.status.send_modify(|s| {
            s.checked_at = Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            );
            s.available_version = newer.then(|| version.clone());
            s.release_url =
                newer.then(|| format!("https://github.com/{REPO}/releases/tag/v{version}"));
        });
        if let Some(requested) = install {
            ensure!(
                newer && requested == version,
                "Release changed; check again"
            );
            let root = self
                .root
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Development install disabled"))?;
            eligible(&root.join("herdr-webapp"))?;
            let mut lock = Lock::acquire(root)?;
            ensure!(
                fs::symlink_metadata(sidecar(root, ".old")).is_err(),
                "Previous update backup exists and could not be safely retired; inspect its managed receipt, ownership and contents before updating again"
            );
            let stage = sidecar(root, &format!(".stage-{}", uuid::Uuid::new_v4()));
            fs::DirBuilder::new().mode(0o700).create(&stage)?;
            let result = (|| -> Result<()> {
                let name = format!("herdr-webapp-v{version}-{}.tar.gz", target());
                let asset = |name: &str| -> Result<u64> {
                    let matches: Vec<_> =
                        release.assets.iter().filter(|a| a.name == name).collect();
                    ensure!(matches.len() == 1, "Release asset missing or duplicated");
                    Ok(matches[0].id)
                };
                let sums = gh(
                    &format!("repos/{REPO}/releases/assets/{}", asset("SHA256SUMS")?),
                    true,
                    16 * 1024,
                )?;
                let expected = checksum(&sums, &name)?;
                let bytes = gh(
                    &format!("repos/{REPO}/releases/assets/{}", asset(&name)?),
                    true,
                    64 * 1024 * 1024,
                )?;
                let actual: String = Sha256::digest(&bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                ensure!(actual == expected, "Release checksum mismatch");
                let archive = stage.join("release.tar.gz");
                write_private(&archive, &bytes, 0o600)?;
                let names = String::from_utf8(tar(&archive, "-tzf", None, 16 * 1024)?)?;
                let mut seen = HashSet::new();
                for name in names.lines() {
                    ensure!(
                        archive_name(name) && seen.insert(name),
                        "Unsafe archive entry"
                    );
                }
                ensure!(FILES.iter().all(|n| seen.contains(n)), "Incomplete archive");
                let verbose = String::from_utf8(tar(&archive, "-tvzf", None, 32 * 1024)?)?;
                ensure!(
                    verbose.lines().count() == seen.len()
                        && names.lines().zip(verbose.lines()).all(|(name, line)| {
                            line.as_bytes().first()
                                == Some(&if name.ends_with('/') { b'd' } else { b'-' })
                        }),
                    "Unsafe archive entry type"
                );
                for name in FILES {
                    let bytes = tar(&archive, "-xOzf", Some(name), 128 * 1024 * 1024)?;
                    let path = stage.join(name);
                    if let Some(p) = path.parent() {
                        fs::DirBuilder::new()
                            .recursive(true)
                            .mode(0o700)
                            .create(p)?;
                    }
                    write_private(
                        &path,
                        &bytes,
                        if name.ends_with(".ts") { 0o600 } else { 0o700 },
                    )?;
                }
                fs::remove_file(archive)?;
                let mut c = Command::new(stage.join("herdr-webapp"));
                c.arg("--version");
                ensure!(
                    output(c, 1024)? == format!("herdr-webapp {version}\n").as_bytes(),
                    "Candidate version does not match"
                );
                write_private(
                    &stage.join("install.json"),
                    &serde_json::to_vec(&receipt(&version))?,
                    0o600,
                )?;
                let plan = Plan {
                    root: root.clone(),
                    stage: stage.clone(),
                    version,
                    args: self.args.lock().unwrap().clone(),
                    identity: RestartIdentity::capture(),
                };
                let path = stage.join("update-plan.json");
                write_private(&path, &serde_json::to_vec(&plan)?, 0o600)?;
                *self.pending_helper.lock().unwrap() = Some(path);
                // Transfer the lock across exec; no child or shell job is created.
                lock.held = false;
                self.phase(
                    "restarting",
                    "Verified update ready; restarting the web server",
                );
                self.shutdown.notify_one();
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_dir_all(stage);
            }
            result?;
        } else {
            self.phase(if newer{"available"}else{"idle"},if newer {if self.root.is_some(){"A stable release is available"}else{"A release is available; source builds cannot self-install. Use the release installer."}}else{"Already running the latest version"});
        }
        Ok(())
    }
}
/// Invoked only by a staged, checksum-verified candidate, never from HTTP parameters.
pub fn helper(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "Invalid helper plan");
    private(path.parent().unwrap(), true)?;
    let p: Plan = serde_json::from_slice(&read_private(path, 16 * 1024)?)?;
    ensure!(
        p.version == VERSION
            && semver(&p.version).is_ok()
            && p.stage == path.parent().unwrap()
            && fs::canonicalize(std::env::current_exe()?)? == p.stage.join("herdr-webapp"),
        "Invalid helper candidate"
    );
    private(&p.stage, true)?;
    private(&p.root, true)?;
    safe_parent(p.root.parent().unwrap())?;
    ensure!(
        fs::canonicalize(&p.root)? == p.root && fs::canonicalize(&p.stage)? == p.stage,
        "Invalid helper directories"
    );
    ensure!(
        p.stage.parent() == p.root.parent()
            && p.root.file_name().is_some()
            && p.stage
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!(
                    "{}.stage-",
                    p.root.file_name().unwrap().to_string_lossy()
                )),
        "Invalid staging path"
    );
    private(&sidecar(&p.root, ".update-lock"), true)?;
    // The first exec already destroyed old runtime tasks and closed CLOEXEC
    // listener/core/auth descriptors, without changing the controlling terminal.
    p.identity.validate()?;
    let backup = sidecar(&p.root, ".old");
    ensure!(
        !backup.exists() && fs::symlink_metadata(&backup).is_err(),
        "Backup exists; recover it manually before updating"
    );
    fs::remove_file(path)?;
    swap_directories(&p.root, &p.stage, &backup)?;
    let _pid = register_pid(&p.root)?;
    fs::remove_dir(sidecar(&p.root, ".update-lock"))?;
    let error = Command::new(p.root.join("herdr-webapp"))
        .args(&p.args)
        .exec();
    // Preserve both versions if exec fails; restore the known-good directory.
    let _ = fs::rename(&p.root, &p.stage);
    let _ = fs::rename(&backup, &p.root);
    Err(error.into())
}
fn retire_backup(root: &Path) -> Result<()> {
    retire_backup_for_version(root, VERSION)
}
fn retire_backup_for_version(root: &Path, version: &str) -> Result<()> {
    let backup = sidecar(root, ".old");
    match fs::symlink_metadata(&backup) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    private(&backup, true)?;
    ensure!(
        fs::canonicalize(&backup)? == backup,
        "Unsafe backup directory"
    );
    let receipt: Receipt =
        serde_json::from_slice(&read_private(&backup.join("install.json"), 4096)?)?;
    ensure!(
        receipt.schema_version == 1
            && receipt.repository == REPO
            && receipt.target == target()
            && semver(&receipt.version)? < semver(version)?,
        "Backup is not an older managed release"
    );
    // Validate every node before removing anything. Do not follow links or remove
    // unrecognized user content merely because an install receipt is present.
    fn validate_tree(directory: &Path, prefix: &str, seen: &mut HashSet<String>) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Unsafe backup name"))?;
            let relative = format!("{prefix}{name}");
            let m = fs::symlink_metadata(entry.path())?;
            ensure!(
                m.uid() == unsafe { libc::getuid() }
                    && m.mode() & 0o022 == 0
                    && !m.file_type().is_symlink(),
                "Unsafe backup entry"
            );
            if m.is_dir() {
                ensure!(
                    ["pi-extension", "scripts"].contains(&relative.as_str()),
                    "Unknown backup directory"
                );
                validate_tree(&entry.path(), &format!("{relative}/"), seen)?;
            } else {
                ensure!(
                    m.is_file()
                        && m.nlink() == 1
                        && (FILES.contains(&relative.as_str())
                            || ["install.json", "LICENSE"].contains(&relative.as_str())),
                    "Unknown or unsafe backup file"
                );
                seen.insert(relative);
            }
        }
        Ok(())
    }
    let mut seen = HashSet::new();
    validate_tree(&backup, "", &mut seen)?;
    ensure!(
        FILES.iter().all(|name| seen.contains(*name)),
        "Incomplete managed backup"
    );
    fs::remove_dir_all(backup)?;
    Ok(())
}
fn swap_directories(root: &Path, stage: &Path, backup: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(backup).is_err(),
        "Backup already exists"
    );
    fs::rename(root, backup)?;
    if let Err(e) = fs::rename(stage, root) {
        fs::rename(backup, root).map_err(|_| {
            anyhow::anyhow!(
                "Activation and rollback failed; preserved backup requires manual recovery"
            )
        })?;
        return Err(e.into());
    }
    Ok(())
}
fn authorize(app: &Shared, headers: &HeaderMap, uri: &Uri, post: bool) -> Result<(), ApiError> {
    app.auth.require(headers)?;
    let count = headers.get_all(axum::http::header::ORIGIN).iter().count();
    if count > 1 || (post && count != 1) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "An exact same Origin is required",
        ));
    }
    if count == 1 {
        app.auth.same_origin(headers, true)?;
    }
    if uri.query().is_some() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Update endpoints do not accept query parameters",
        ));
    }
    Ok(())
}
pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/updates", get(status))
        .route("/api/updates/events", get(events))
        .route("/api/updates/check", post(check))
        .route("/api/updates/install", post(install))
}
async fn status(
    State(app): State<Shared>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Json<UpdateStatus>, ApiError> {
    authorize(&app, &headers, &uri, false)?;
    Ok(Json(app.updates.status.borrow().clone()))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Install {
    version: String,
}
async fn check(
    State(app): State<Shared>,
    headers: HeaderMap,
    uri: Uri,
    Json(_): Json<Empty>,
) -> Result<(StatusCode, Json<UpdateStatus>), ApiError> {
    authorize(&app, &headers, &uri, true)?;
    app.updates.begin().map_err(|_| {
        error(
            StatusCode::CONFLICT,
            "An update operation is already running",
        )
    })?;
    app.updates.clone().launch(None);
    Ok((
        StatusCode::ACCEPTED,
        Json(app.updates.status.borrow().clone()),
    ))
}
async fn install(
    State(app): State<Shared>,
    headers: HeaderMap,
    uri: Uri,
    Json(body): Json<Install>,
) -> Result<(StatusCode, Json<UpdateStatus>), ApiError> {
    authorize(&app, &headers, &uri, true)?;
    let s = app.updates.status.borrow().clone();
    if !s.can_install || s.available_version.as_deref() != Some(&body.version) {
        return Err(error(
            StatusCode::CONFLICT,
            "Check for updates first; requested version must match the available managed release",
        ));
    }
    app.updates.begin().map_err(|_| {
        error(
            StatusCode::CONFLICT,
            "An update operation is already running",
        )
    })?;
    app.updates.clone().launch(Some(body.version));
    Ok((
        StatusCode::ACCEPTED,
        Json(app.updates.status.borrow().clone()),
    ))
}
async fn events(
    State(app): State<Shared>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    authorize(&app, &headers, &uri, false)?;
    let mut rx = app.updates.status.subscribe();
    Ok(Sse::new(async_stream::stream!{loop{if !app.auth.authorized(&headers){break;}let data=serde_json::to_string(&*rx.borrow_and_update()).unwrap();yield Ok(Event::default().event("update").data(data));tokio::select!{r=rx.changed()=>if r.is_err(){break;},_=tokio::time::sleep(Duration::from_secs(15))=>{}}}}).keep_alive(KeepAlive::default()))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn private_tmp() -> tempfile::TempDir {
        tempfile::Builder::new()
            .tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap())
            .unwrap()
    }
    #[test]
    fn release_targets_match_distribution_manifest() {
        let manifest: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../distribution/release-targets.json")).unwrap();
        assert_eq!(manifest.len(), 8);
        let mut seen = HashSet::new();
        for entry in manifest {
            let published = entry["target"].as_str().unwrap();
            assert!(seen.insert(published.to_owned()), "duplicate target");
            assert_eq!(release_target(published), published);
        }
    }
    #[test]
    fn release_target_identity_preserves_architecture_and_libc() {
        for published in [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "armv7-unknown-linux-gnueabihf",
            "i686-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
        ] {
            assert_eq!(release_target(published), published);
        }
        for unpublished in [
            "arm-unknown-linux-gnueabi",
            "arm-unknown-linux-gnueabihf",
            "armv6-unknown-linux-gnueabihf",
            "armv7-unknown-linux-gnueabi",
            "armv7-unknown-linux-musleabihf",
            "i686-unknown-linux-musl",
            "x86_64-unknown-linux-uclibc",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-freebsd",
            "",
        ] {
            assert_eq!(release_target(unpublished), "unsupported");
        }
        assert_eq!(target(), release_target(env!("HERDR_BUILD_TARGET")));
        assert_eq!(receipt("1.2.3").target, target());
        #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
        assert_eq!(target(), "x86_64-unknown-linux-gnu");
        #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "musl"))]
        assert_eq!(target(), "x86_64-unknown-linux-musl");
    }
    #[test]
    fn restart_identity_keeps_original_terminal_job() {
        let identity = RestartIdentity::capture();
        identity.validate().unwrap();
        let mut wrong = identity.clone();
        wrong.pid = wrong.pid.wrapping_add(1);
        assert!(wrong.validate().is_err());
        let mut wrong = identity.clone();
        wrong.process_group = -1;
        assert!(wrong.validate().is_err());
        let mut wrong = identity;
        wrong.session = -1;
        assert!(wrong.validate().is_err());
    }
    #[tokio::test]
    async fn runtime_lock_and_listener_descriptors_close_on_exec() {
        use std::os::fd::AsRawFd;
        let d = private_tmp();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(d.path().join("sessions.lock"))
            .unwrap();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        for fd in [lock.as_raw_fd(), listener.as_raw_fd()] {
            assert_ne!(
                unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
        }
    }
    #[test]
    fn versions() {
        assert!(semver("0.0.1").unwrap() > semver("0.0.0").unwrap());
        for s in ["1.2", "01.2.3", "1.2.3-rc1", "1.2.3+foo", "../x"] {
            assert!(semver(s).is_err());
        }
    }
    #[test]
    fn names() {
        for n in [
            "/herdr-webapp",
            "../herdr-webapp",
            "./herdr-webapp",
            "pi-extension/../herdr-webapp",
            "scripts/evil",
        ] {
            assert!(!archive_name(n));
        }
        for n in FILES {
            assert!(archive_name(n));
        }
        assert!(archive_name("LICENSE"));
        assert!(!archive_name("LICENSE/"));
        assert!(!archive_name("../LICENSE"));
    }
    #[test]
    fn sums() {
        let s = format!("{}  file\n", "a".repeat(64));
        assert_eq!(checksum(s.as_bytes(), "file").unwrap(), "a".repeat(64));
        assert!(checksum(s.as_bytes(), "other").is_err());
        assert!(checksum(format!("{s}{s}").as_bytes(), "file").is_err());
    }
    #[test]
    fn unmanaged() {
        let d = private_tmp();
        write_private(&d.path().join("herdr-webapp"), b"test", 0o700).unwrap();
        assert!(eligible(&d.path().join("herdr-webapp")).is_err());
    }
    #[test]
    fn receipt_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let d = private_tmp();
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let root = d.path().join("managed");
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let exe = root.join("herdr-webapp");
        write_private(&exe, b"candidate", 0o700).unwrap();
        let receipt_path = root.join("install.json");
        write_private(
            &receipt_path,
            &serde_json::to_vec(&receipt("1.0.0")).unwrap(),
            0o600,
        )
        .unwrap();
        assert_eq!(eligible_for_version(&exe, "1.0.0").unwrap(), root);
        assert!(eligible_for_version(&exe, "").is_err());
        fs::remove_file(&receipt_path).unwrap();
        write_private(
            &receipt_path,
            &serde_json::to_vec(&receipt("999.0.0")).unwrap(),
            0o600,
        )
        .unwrap();
        assert!(eligible(&exe).is_err());
        fs::remove_file(&receipt_path).unwrap();
        std::os::unix::fs::symlink(&exe, &receipt_path).unwrap();
        assert!(eligible(&exe).is_err());
    }
    fn backup_fixture(root: &Path, version: &str) -> PathBuf {
        let backup = sidecar(root, ".old");
        fs::DirBuilder::new().mode(0o700).create(&backup).unwrap();
        for name in FILES {
            let file = backup.join(name);
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(file.parent().unwrap())
                .unwrap();
            write_private(&file, b"managed file", 0o700).unwrap();
        }
        write_private(
            &backup.join("install.json"),
            &serde_json::to_vec(&receipt(version)).unwrap(),
            0o600,
        )
        .unwrap();
        backup
    }
    #[test]
    fn ready_retires_only_validated_older_backup_and_allows_next_update() {
        let d = private_tmp();
        let root = d.path().join("app");
        let backup = backup_fixture(&root, "0.0.0");
        write_private(&backup.join("LICENSE"), b"license", 0o600).unwrap();
        retire_backup_for_version(&root, "1.0.0").unwrap();
        assert!(!backup.exists());
        retire_backup_for_version(&root, "1.0.0").unwrap(); // readiness is idempotent
        fs::create_dir(&root).unwrap();
        let stage = d.path().join("stage");
        fs::create_dir(&stage).unwrap();
        swap_directories(&root, &stage, &backup).unwrap(); // next update is no longer blocked
        assert!(backup.is_dir());
    }
    #[test]
    fn backup_retirement_refuses_unsafe_or_unmanaged_content_without_deletion() {
        let d = private_tmp();
        let root = d.path().join("app");
        let backup = backup_fixture(&root, VERSION);
        assert!(retire_backup(&root).is_err()); // not an older release
        assert!(backup.join("herdr-webapp").exists());
        fs::remove_file(backup.join("install.json")).unwrap();
        write_private(
            &backup.join("install.json"),
            &serde_json::to_vec(&receipt("0.0.0")).unwrap(),
            0o600,
        )
        .unwrap();
        write_private(&backup.join("user-data"), b"preserve", 0o600).unwrap();
        assert!(retire_backup(&root).is_err());
        assert!(backup.join("user-data").exists());
        fs::remove_file(backup.join("user-data")).unwrap();
        fs::remove_file(backup.join("herdr-webapp")).unwrap();
        std::os::unix::fs::symlink("/nonexistent", backup.join("herdr-webapp")).unwrap();
        assert!(retire_backup(&root).is_err());
        assert!(backup.join("install.json").exists());
        let other = d.path().join("other");
        fs::rename(&backup, &other).unwrap();
        std::os::unix::fs::symlink(&other, &backup).unwrap();
        assert!(retire_backup(&root).is_err());
        assert!(other.join("install.json").exists());
    }
    #[test]
    fn activation_rolls_back() {
        let d = private_tmp();
        let root = d.path().join("app");
        let stage = d.path().join("stage");
        let backup = d.path().join("backup");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("marker"), b"old").unwrap();
        assert!(swap_directories(&root, &stage, &backup).is_err());
        assert_eq!(fs::read(root.join("marker")).unwrap(), b"old");
        assert!(!backup.exists());
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("marker"), b"new").unwrap();
        swap_directories(&root, &stage, &backup).unwrap();
        assert_eq!(fs::read(root.join("marker")).unwrap(), b"new");
        assert_eq!(fs::read(backup.join("marker")).unwrap(), b"old");
    }
    #[tokio::test]
    async fn route_security() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        let app = Arc::new(crate::App {
            core: crate::core::Core::new("/nonexistent".into()),
            auth: crate::auth::Auth::new("a".repeat(64), None),
            updates: Updater::development(),
        });
        let router = crate::router(app);
        for (method, path, origin, body, expected) in [
            ("GET", "/api/updates", None, None, 200),
            ("GET", "/api/updates?repository=evil", None, None, 400),
            ("GET", "/api/updates", Some("http://evil"), None, 403),
            ("POST", "/api/updates/check", None, Some("{}"), 403),
            (
                "POST",
                "/api/updates/check",
                Some("http://localhost"),
                Some("{\"path\":\"/evil\"}"),
                422,
            ),
            (
                "POST",
                "/api/updates/install",
                Some("http://localhost"),
                Some("{\"version\":\"0.0.1\",\"version\":\"0.0.2\"}"),
                422,
            ),
            (
                "POST",
                "/api/updates/install",
                Some("http://localhost"),
                Some("{\"version\":\"99.0.0\"}"),
                409,
            ),
        ] {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {}", "a".repeat(64)))
                .header("host", "localhost");
            if let Some(o) = origin {
                request = request.header("origin", o);
            }
            if body.is_some() {
                request = request.header("content-type", "application/json");
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::from(body.unwrap_or(""))).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status().as_u16(),
                expected,
                "{method} {path} {body:?}"
            );
        }
        let response = router
            .oneshot(Request::get("/api/updates").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    #[test]
    fn lock_exclusive() {
        let d = private_tmp();
        let root = d.path().join("app");
        let l = Lock::acquire(&root).unwrap();
        assert!(Lock::acquire(&root).is_err());
        drop(l);
        assert!(Lock::acquire(&root).is_ok());
    }
}
