//! Verified, memory-backed public release downloads. No network or installer credentials.
use anyhow::{Result, bail, ensure};
use axum::{
    Router,
    body::Body,
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
    path::Path,
    sync::Arc,
};

const LEGACY_TARGETS: [&str; 3] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
];
const TARGETS: [&str; 8] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "armv7-unknown-linux-gnueabihf",
    "i686-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];
const MAX_TOTAL: u64 = 256 * 1024 * 1024;
const MAX_FILE: u64 = 128 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    protocol_version: u32,
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

pub struct ReleaseCatalog {
    version: String,
    manifest: Vec<u8>,
    binaries: HashMap<String, axum::body::Bytes>,
}

fn stable(version: &str) -> bool {
    version.len() <= 64
        && version.split('.').count() == 3
        && version.split('.').all(|p| {
            !p.is_empty()
                && (p == "0" || !p.starts_with('0'))
                && p.bytes().all(|c| c.is_ascii_digit())
        })
}

fn safe_metadata(file: &File, directory: bool) -> Result<()> {
    let m = file.metadata()?;
    ensure!(
        m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o022 == 0,
        "unsafe catalog ownership or permissions"
    );
    ensure!(
        if directory {
            m.is_dir()
        } else {
            m.is_file() && m.nlink() == 1
        },
        "unsafe catalog entry"
    );
    Ok(())
}

// Open every descendant relative to an already-open directory, never following links.
fn open_at(parent: &File, name: &str, directory: bool) -> Result<File> {
    let name = std::ffi::CString::new(name)?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    ensure!(fd >= 0, "cannot open catalog entry");
    let file = unsafe { File::from_raw_fd(fd) };
    safe_metadata(&file, directory)?;
    Ok(file)
}
fn bounded_read(mut file: File, limit: u64) -> Result<Vec<u8>> {
    ensure!(file.metadata()?.len() <= limit, "catalog entry too large");
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "catalog entry too large");
    safe_metadata(&file, false)?;
    Ok(bytes)
}

impl ReleaseCatalog {
    /// Load once at startup. Retained verified bytes are independent of later disk changes.
    pub fn load(directory: &Path, expected_version: &str) -> Result<Self> {
        ensure!(
            stable(expected_version),
            "expected release version is not stable"
        );
        // Resolve path components without following symlinks (including the root path).
        let mut root = File::open(if directory.is_absolute() { "/" } else { "." })?;
        for part in directory.components() {
            match part {
                std::path::Component::RootDir | std::path::Component::CurDir => (),
                std::path::Component::Normal(name) => {
                    let name = std::ffi::CString::new(name.as_encoded_bytes())?;
                    let fd = unsafe {
                        libc::openat(
                            root.as_raw_fd(),
                            name.as_ptr(),
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                        )
                    };
                    ensure!(fd >= 0, "cannot open catalog directory");
                    root = unsafe { File::from_raw_fd(fd) };
                }
                _ => bail!("unsafe catalog directory path"),
            }
        }
        safe_metadata(&root, true)?;
        let raw = bounded_read(open_at(&root, "manifest.json", false)?, 65536)?;
        let manifest: Manifest = serde_json::from_slice(&raw)?;
        ensure!(
            manifest.schema_version == 1 && manifest.protocol_version == 1,
            "unsupported catalog schema or protocol"
        );
        ensure!(
            manifest.version == expected_version && stable(&manifest.version),
            "catalog version mismatch"
        );
        let targets: &[&str] = match manifest.assets.len() {
            3 => &LEGACY_TARGETS,
            8 => &TARGETS,
            _ => bail!("incomplete catalog targets"),
        };
        let version_dir = open_at(&root, &manifest.version, true)?;
        let mut binaries = HashMap::new();
        let mut total = 0u64;
        for asset in &manifest.assets {
            ensure!(
                targets.contains(&asset.target.as_str()) && !binaries.contains_key(&asset.target),
                "invalid or duplicate catalog target"
            );
            ensure!(
                asset.url == format!("/client/{}/{}/herdr-webapp", manifest.version, asset.target),
                "unsafe catalog asset URL"
            );
            ensure!(
                asset.size > 0 && asset.size <= MAX_FILE,
                "invalid catalog asset size"
            );
            total = total
                .checked_add(asset.size)
                .ok_or_else(|| anyhow::anyhow!("catalog size overflow"))?;
            ensure!(total <= MAX_TOTAL, "catalog too large");
            ensure!(
                asset.sha256.len() == 64
                    && asset
                        .sha256
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "invalid catalog digest"
            );
            let target_dir = open_at(&version_dir, &asset.target, true)?;
            let bytes = bounded_read(open_at(&target_dir, "herdr-webapp", false)?, asset.size)?;
            ensure!(
                bytes.len() as u64 == asset.size
                    && Sha256::digest(&bytes)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                        == asset.sha256,
                "catalog integrity check failed"
            );
            binaries.insert(asset.target.clone(), bytes.into());
        }
        Ok(Self {
            version: manifest.version.clone(),
            manifest: serde_json::to_vec(&manifest)?,
            binaries,
        })
    }
}

pub fn router(catalog: Arc<ReleaseCatalog>) -> Router {
    Router::new()
        .route("/client/manifest.json", get(manifest))
        .route("/client/{version}/{target}/herdr-webapp", get(binary))
        .with_state(catalog)
}
/// Explicit unconfigured state; does not advertise assets or proxy downloads.
pub fn unavailable_router() -> Router {
    Router::new().route(
        "/client/manifest.json",
        get(|| async {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [
                    (header::CACHE_CONTROL, "no-store"),
                    (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                ],
                "Client downloads are unavailable: no approved release catalog is configured.\n",
            )
        }),
    )
}
async fn manifest(State(c): State<Arc<ReleaseCatalog>>) -> Response {
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
    State(c): State<Arc<ReleaseCatalog>>,
    RoutePath((version, target)): RoutePath<(String, String)>,
) -> Response {
    if version != c.version {
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

    fn fixture() -> tempfile::TempDir {
        let temp = tempfile::Builder::new()
            .tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
            .unwrap();
        std::fs::set_permissions(
            temp.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let mut assets = Vec::new();
        for target in TARGETS {
            let dir = temp.path().join("1.2.3").join(target);
            std::fs::create_dir_all(&dir).unwrap();
            for path in [dir.clone(), temp.path().join("1.2.3")] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let file = dir.join("herdr-webapp");
            std::fs::write(&file, b"verified binary").unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
            assets.push(Asset {
                target: target.into(),
                url: format!("/client/1.2.3/{target}/herdr-webapp"),
                sha256: Sha256::digest(b"verified binary")
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
                size: 15,
            });
        }
        let manifest = Manifest {
            schema_version: 1,
            protocol_version: 1,
            version: "1.2.3".into(),
            assets,
        };
        let file = temp.path().join("manifest.json");
        std::fs::write(&file, serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
        temp
    }
    fn edit_manifest(root: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
        let file = root.join("manifest.json");
        let mut value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        edit(&mut value);
        std::fs::write(file, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    #[test]
    fn accepts_only_exact_legacy_or_full_cohorts() {
        let declared: serde_json::Value =
            serde_json::from_str(include_str!("../distribution/release-targets.json")).unwrap();
        let declared: Vec<&str> = declared
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["target"].as_str().unwrap())
            .collect();
        assert_eq!(declared, TARGETS);
        for count in 0..=8 {
            let temp = fixture();
            edit_manifest(temp.path(), |m| {
                m["assets"].as_array_mut().unwrap().truncate(count);
            });
            // The first three of the new ordering are not the legacy three.
            assert_eq!(
                ReleaseCatalog::load(temp.path(), "1.2.3").is_ok(),
                count == 8
            );
        }
        let temp = fixture();
        edit_manifest(temp.path(), |m| {
            m["assets"]
                .as_array_mut()
                .unwrap()
                .retain(|a| LEGACY_TARGETS.contains(&a["target"].as_str().unwrap()));
        });
        assert!(ReleaseCatalog::load(temp.path(), "1.2.3").is_ok());
    }
    #[test]
    fn rejects_integrity_version_and_schema_errors() {
        for bad in [
            "version",
            "digest",
            "url",
            "duplicate",
            "unknown",
            "size",
            "target",
            "protocol",
        ] {
            let temp = fixture();
            edit_manifest(temp.path(), |m| match bad {
                "version" => m["version"] = "2.0.0".into(),
                "digest" => m["assets"][0]["sha256"] = "0".repeat(64).into(),
                "url" => m["assets"][0]["url"] = "https://private.invalid/secret".into(),
                "duplicate" => m["assets"][1] = m["assets"][0].clone(),
                "unknown" => m["installerToken"] = "private".into(),
                "size" => m["assets"][0]["size"] = (MAX_FILE + 1).into(),
                "target" => m["assets"][0]["target"] = "../escape".into(),
                _ => m["protocolVersion"] = 2.into(),
            });
            assert!(ReleaseCatalog::load(temp.path(), "1.2.3").is_err(), "{bad}");
        }
    }
    #[test]
    fn rejects_links_and_writable_entries() {
        for bad in ["symlink", "hardlink", "writable", "directory", "rootlink"] {
            let temp = fixture();
            let file = temp
                .path()
                .join("1.2.3")
                .join(TARGETS[0])
                .join("herdr-webapp");
            match bad {
                "symlink" => {
                    std::fs::rename(&file, temp.path().join("original")).unwrap();
                    symlink(temp.path().join("original"), &file).unwrap();
                }
                "hardlink" => std::fs::hard_link(&file, temp.path().join("extra")).unwrap(),
                "writable" => {
                    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o620)).unwrap()
                }
                "directory" => {
                    std::fs::remove_file(&file).unwrap();
                    std::fs::create_dir(&file).unwrap();
                }
                _ => {
                    symlink(temp.path(), temp.path().join("alias")).unwrap();
                    assert!(ReleaseCatalog::load(&temp.path().join("alias"), "1.2.3").is_err());
                    continue;
                }
            }
            assert!(ReleaseCatalog::load(temp.path(), "1.2.3").is_err(), "{bad}");
        }
    }
    #[tokio::test]
    async fn serves_retained_bytes_and_only_exact_assets() {
        let temp = fixture();
        let catalog = Arc::new(ReleaseCatalog::load(temp.path(), "1.2.3").unwrap());
        std::fs::remove_dir_all(temp.path().join("1.2.3")).unwrap();
        let app = router(catalog);
        for target in TARGETS {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/client/1.2.3/{target}/herdr-webapp"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{target}");
            let bytes = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(&bytes[..], b"verified binary");
        }
        let uri = format!("/client/1.2.3/{}/herdr-webapp", TARGETS[0]);
        let response = app
            .clone()
            .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
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
        let data = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(&data[..], b"verified binary");
        for uri in [
            "/client/manifest.json",
            "/client/9.9.9/x86_64-unknown-linux-gnu/herdr-webapp",
            "/client/1.2.3/unsupported/herdr-webapp",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            if uri.ends_with("manifest.json") {
                assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
                assert_eq!(response.status(), StatusCode::OK);
            } else {
                assert_eq!(response.status(), StatusCode::NOT_FOUND);
            }
        }
    }
    #[tokio::test]
    async fn unconfigured_is_explicit() {
        let response = unavailable_router()
            .oneshot(
                Request::builder()
                    .uri("/client/manifest.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
}
