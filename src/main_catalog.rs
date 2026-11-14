//! Verified immutable snapshots of private main-channel native client artifacts.
use anyhow::{Result, bail, ensure};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path as RoutePath, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path},
    sync::Arc,
};

const TARGETS: [&str; 3] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
];
const MAX_FILE: u64 = 128 * 1024 * 1024;
const MAX_TOTAL: u64 = 256 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    protocol_version: u32,
    channel: String,
    revision: String,
    version: String,
    assets: Vec<Asset>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    target: String,
    url: String,
    sha256: String,
    size: u64,
}

pub struct MainCatalog {
    revision: String,
    manifest: Vec<u8>,
    binaries: HashMap<String, Bytes>,
}

fn lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn revision_valid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && lower_hex(value)
}
fn stable(value: &str) -> bool {
    value.len() <= 64
        && value.split('.').count() == 3
        && value.split('.').all(|p| {
            !p.is_empty()
                && (p == "0" || !p.starts_with('0'))
                && p.bytes().all(|c| c.is_ascii_digit())
        })
}
fn safe_metadata(file: &File, mode: u32, directory: bool) -> Result<()> {
    let m = file.metadata()?;
    ensure!(
        m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o7777 == mode,
        "unsafe main catalog ownership or permissions"
    );
    ensure!(
        if directory {
            m.is_dir()
        } else {
            m.is_file() && m.nlink() == 1
        },
        "unsafe main catalog entry"
    );
    Ok(())
}
fn open_at(parent: &File, name: &str, mode: u32, directory: bool) -> Result<File> {
    let name = std::ffi::CString::new(name)?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    ensure!(fd >= 0, "cannot open main catalog entry");
    let file = unsafe { File::from_raw_fd(fd) };
    safe_metadata(&file, mode, directory)?;
    Ok(file)
}
fn bounded_read(mut file: File, limit: u64, mode: u32) -> Result<Vec<u8>> {
    ensure!(
        file.metadata()?.len() <= limit,
        "main catalog entry too large"
    );
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "main catalog entry too large");
    safe_metadata(&file, mode, false)?;
    Ok(bytes)
}
impl MainCatalog {
    /// Read only bounded, verified files; serving never accesses the filesystem.
    pub fn load(directory: &Path, expected_revision: &str) -> Result<Self> {
        ensure!(
            revision_valid(expected_revision),
            "invalid expected main revision"
        );
        // Walk *all* ancestors without following links. Ancestors such as /tmp
        // need not be private, but the artifact root and descendants must be.
        let mut root = File::open(if directory.is_absolute() { "/" } else { "." })?;
        for part in directory.components() {
            match part {
                Component::RootDir | Component::CurDir => (),
                Component::Normal(name) => {
                    let name = std::ffi::CString::new(name.as_encoded_bytes())?;
                    let fd = unsafe {
                        libc::openat(
                            root.as_raw_fd(),
                            name.as_ptr(),
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                        )
                    };
                    ensure!(fd >= 0, "cannot open main catalog directory");
                    root = unsafe { File::from_raw_fd(fd) };
                }
                _ => bail!("unsafe main catalog directory path"),
            }
        }
        safe_metadata(&root, 0o700, true)?;
        let raw = bounded_read(open_at(&root, "manifest.json", 0o600, false)?, 65536, 0o600)?;
        let manifest: Manifest = serde_json::from_slice(&raw)?;
        ensure!(
            manifest.schema_version == 2 && manifest.protocol_version == 1,
            "unsupported main catalog schema or protocol"
        );
        ensure!(manifest.channel == "main", "invalid main catalog channel");
        ensure!(
            revision_valid(&manifest.revision) && manifest.revision == expected_revision,
            "main catalog revision mismatch"
        );
        ensure!(
            stable(&manifest.version),
            "main catalog version is not stable"
        );
        ensure!(
            !manifest.assets.is_empty() && manifest.assets.len() <= TARGETS.len(),
            "invalid main catalog asset count"
        );
        let revision_dir = open_at(&root, &manifest.revision, 0o700, true)?;
        let mut binaries = HashMap::new();
        let mut total = 0u64;
        for asset in &manifest.assets {
            ensure!(
                TARGETS.contains(&asset.target.as_str()) && !binaries.contains_key(&asset.target),
                "invalid or duplicate main catalog target"
            );
            ensure!(
                asset.url
                    == format!(
                        "/main/client/{}/{}/herdr-webapp",
                        manifest.revision, asset.target
                    ),
                "unsafe main catalog asset URL"
            );
            ensure!(
                asset.size > 0 && asset.size <= MAX_FILE,
                "invalid main catalog asset size"
            );
            total = total
                .checked_add(asset.size)
                .ok_or_else(|| anyhow::anyhow!("main catalog size overflow"))?;
            ensure!(total <= MAX_TOTAL, "main catalog too large");
            ensure!(
                asset.sha256.len() == 64 && lower_hex(&asset.sha256),
                "invalid main catalog digest"
            );
            let target_dir = open_at(&revision_dir, &asset.target, 0o700, true)?;
            let bytes = bounded_read(
                open_at(&target_dir, "herdr-webapp", 0o700, false)?,
                asset.size,
                0o700,
            )?;
            let digest = Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            ensure!(
                bytes.len() as u64 == asset.size && digest == asset.sha256,
                "main catalog integrity check failed"
            );
            binaries.insert(asset.target.clone(), Bytes::from(bytes));
        }
        Ok(Self {
            revision: manifest.revision.clone(),
            manifest: serde_json::to_vec(&manifest)?,
            binaries,
        })
    }
}

/// Paths are relative to the parent's /main mount.
pub fn router(catalog: Arc<MainCatalog>) -> Router {
    Router::new()
        .route("/client/manifest.json", get(manifest))
        .route("/client/{revision}/{target}/herdr-webapp", get(binary))
        .with_state(catalog)
}
async fn manifest(State(c): State<Arc<MainCatalog>>) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        c.manifest.clone(),
    )
        .into_response()
}
async fn binary(
    State(c): State<Arc<MainCatalog>>,
    RoutePath((revision, target)): RoutePath<(String, String)>,
) -> Response {
    if revision != c.revision {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(bytes) = c.binaries.get(&target) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"herdr-webapp\"",
            ),
        ],
        Body::from(bytes.clone()),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use tower::ServiceExt;

    const REV: &str = "0123456789abcdef0123456789abcdef01234567";
    const DATA: &[u8] = b"verified native executable";
    fn mode(path: &Path, value: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(value)).unwrap();
    }
    fn fixture() -> tempfile::TempDir {
        let temp = tempfile::Builder::new()
            .tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
            .unwrap();
        mode(temp.path(), 0o700);
        let revision = temp.path().join(REV);
        let target = revision.join(TARGETS[0]);
        std::fs::create_dir_all(&target).unwrap();
        mode(&revision, 0o700);
        mode(&target, 0o700);
        let binary = target.join("herdr-webapp");
        std::fs::write(&binary, DATA).unwrap();
        mode(&binary, 0o700);
        let manifest = Manifest {
            schema_version: 2,
            protocol_version: 1,
            channel: "main".into(),
            revision: REV.into(),
            version: "1.2.3".into(),
            assets: vec![Asset {
                target: TARGETS[0].into(),
                url: format!("/main/client/{REV}/{}/herdr-webapp", TARGETS[0]),
                sha256: Sha256::digest(DATA)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect(),
                size: DATA.len() as u64,
            }],
        };
        let path = temp.path().join("manifest.json");
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        mode(&path, 0o600);
        temp
    }
    fn edit_manifest(root: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
        let file = root.join("manifest.json");
        let mut value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        edit(&mut value);
        std::fs::write(file, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    #[test]
    fn accepts_native_only_subset_and_full_revisions() {
        let temp = fixture();
        assert!(MainCatalog::load(temp.path(), REV).is_ok());
        let longer = "a".repeat(64);
        std::fs::rename(temp.path().join(REV), temp.path().join(&longer)).unwrap();
        edit_manifest(temp.path(), |m| {
            m["revision"] = longer.clone().into();
            m["assets"][0]["url"] =
                format!("/main/client/{longer}/{}/herdr-webapp", TARGETS[0]).into();
        });
        assert!(MainCatalog::load(temp.path(), &longer).is_ok());
    }
    #[test]
    fn rejects_invalid_manifests() {
        for bad in [
            "schema",
            "protocol",
            "channel",
            "revision",
            "uppercase",
            "version",
            "prerelease",
            "empty",
            "duplicate",
            "unknown",
            "unknown_asset",
            "target",
            "url",
            "traversal_url",
            "digest",
            "badsha",
            "size",
            "zero",
        ] {
            let temp = fixture();
            edit_manifest(temp.path(), |m| match bad {
                "schema" => m["schemaVersion"] = 1.into(),
                "protocol" => m["protocolVersion"] = 2.into(),
                "channel" => m["channel"] = "release".into(),
                "revision" => m["revision"] = "b".repeat(40).into(),
                "uppercase" => m["revision"] = REV.to_uppercase().into(),
                "version" => m["version"] = "01.2.3".into(),
                "prerelease" => m["version"] = "1.2.3-dev".into(),
                "empty" => m["assets"] = serde_json::json!([]),
                "duplicate" => m["assets"] = serde_json::json!([m["assets"][0], m["assets"][0]]),
                "unknown" => m["secret"] = true.into(),
                "unknown_asset" => m["assets"][0]["extra"] = true.into(),
                "target" => m["assets"][0]["target"] = "wasm32-unknown-unknown".into(),
                "url" => m["assets"][0]["url"] = "https://example.invalid/binary".into(),
                "traversal_url" => {
                    m["assets"][0]["url"] =
                        format!("/main/client/{REV}/{}/../herdr-webapp", TARGETS[0]).into()
                }
                "digest" => m["assets"][0]["sha256"] = "0".repeat(64).into(),
                "badsha" => m["assets"][0]["sha256"] = "G".repeat(64).into(),
                "zero" => m["assets"][0]["size"] = 0.into(),
                _ => m["assets"][0]["size"] = (MAX_FILE + 1).into(),
            });
            assert!(MainCatalog::load(temp.path(), REV).is_err(), "{bad}");
        }
        let temp = fixture();
        for revision in ["", "abc", "../escape", &REV.to_uppercase()] {
            assert!(MainCatalog::load(temp.path(), revision).is_err());
        }
    }
    #[test]
    fn rejects_unsafe_modes_links_and_corruption() {
        for bad in [
            "root",
            "revision",
            "target",
            "manifest",
            "binary",
            "setuid",
            "symlink",
            "manifestlink",
            "hardlink",
            "directory",
            "ancestorlink",
            "parent",
            "corruption",
            "truncated",
        ] {
            let temp = fixture();
            let revision = temp.path().join(REV);
            let target = revision.join(TARGETS[0]);
            let binary = target.join("herdr-webapp");
            match bad {
                "root" => mode(temp.path(), 0o750),
                "revision" => mode(&revision, 0o755),
                "target" => mode(&target, 0o770),
                "manifest" => mode(&temp.path().join("manifest.json"), 0o644),
                "binary" => mode(&binary, 0o600),
                "setuid" => mode(&binary, 0o4700),
                "symlink" | "manifestlink" => {
                    let file = if bad == "symlink" {
                        binary.clone()
                    } else {
                        temp.path().join("manifest.json")
                    };
                    let saved = temp.path().join("saved");
                    std::fs::rename(&file, &saved).unwrap();
                    symlink(&saved, &file).unwrap();
                }
                "hardlink" => std::fs::hard_link(&binary, temp.path().join("extra")).unwrap(),
                "directory" => {
                    std::fs::remove_file(&binary).unwrap();
                    std::fs::create_dir(&binary).unwrap();
                    mode(&binary, 0o700);
                }
                "ancestorlink" => {
                    symlink(temp.path(), temp.path().join("alias")).unwrap();
                    assert!(MainCatalog::load(&temp.path().join("alias"), REV).is_err());
                    continue;
                }
                "parent" => {
                    assert!(MainCatalog::load(&temp.path().join(".."), REV).is_err());
                    continue;
                }
                "corruption" => std::fs::write(&binary, vec![b'x'; DATA.len()]).unwrap(),
                _ => std::fs::write(&binary, b"short").unwrap(),
            }
            assert!(MainCatalog::load(temp.path(), REV).is_err(), "{bad}");
        }
    }
    #[tokio::test]
    async fn router_is_nested_correctly_and_serves_immutable_snapshot() {
        let temp = fixture();
        let catalog = Arc::new(MainCatalog::load(temp.path(), REV).unwrap());
        std::fs::remove_dir_all(temp.path().join(REV)).unwrap();
        let app = Router::new().nest("/main", router(catalog));
        let uri = format!("/main/client/{REV}/{}/herdr-webapp", TARGETS[0]);
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert!(response.headers().contains_key(header::CONTENT_DISPOSITION));
        assert_eq!(
            &axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()[..],
            DATA
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/main/client/manifest.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        let value: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            value["assets"][0]["url"],
            format!("/main/client/{REV}/{}/herdr-webapp", TARGETS[0])
        );
        for uri in [
            "/client/manifest.json".into(),
            format!("/main/client/{REV}/unsupported/herdr-webapp"),
            format!(
                "/main/client/{}/{}/herdr-webapp",
                "b".repeat(40),
                TARGETS[0]
            ),
            format!("/main/client/{REV}/{}/herdr-webapp", TARGETS[1]),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }
}
