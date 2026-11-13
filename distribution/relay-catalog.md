# Approved public relay downloads

This component publishes the **same unified `herdr-webapp` binary** as each private
GitHub release, not a second client build. Repository access stays on the preparing
host. The public router never uses GitHub, credentials, installer tokens, source
paths, or a filesystem fallback.

## Parent integration contract

Add `mod relay_catalog;` and merge one of these state-isolated `Router<()>` values
into the relay's public HTTP router (outside installer-token authentication):

```rust
let catalog = relay_catalog::ReleaseCatalog::load(directory, env!("CARGO_PKG_VERSION"))?;
let downloads = relay_catalog::router(std::sync::Arc::new(catalog));
// Without configuration:
let downloads = relay_catalog::unavailable_router();
```

`load(&Path, &str) -> anyhow::Result<ReleaseCatalog>` must complete successfully
before activating a configured relay. Do not silently downgrade an invalid
configured catalog to an unconfigured relay. Existing dependencies suffice:
`anyhow`, `axum`, `libc`, `serde`, `serde_json`, `sha2`; tests use `tempfile`,
`tokio`, and `tower`. No source-private RustEmbed assets are changed.

- `GET /client/manifest.json`: schemaVersion 1, protocolVersion 1, stable version,
  exactly three assets containing target, same-origin URL, lowercase SHA-256, size.
  `Cache-Control: no-store`. Unconfigured: explanatory sanitized 503, no assets.
- `GET /client/<version>/<target>/herdr-webapp`: only verified catalog entries;
  octet-stream, nosniff, attachment, immutable year-long public caching.
- Unsupported paths/versions/targets: 404. No general filesystem serving.
- Owner must be the relay's effective UID; directory and file permissions must
  forbid group/world writes. Every file must be regular and singly linked.
  Paths cannot traverse symlinks. Checksums, schema, runtime version and sizes
  are verified before the router retains bytes (128 MiB/file, 256 MiB total).
  Subsequent disk mutations cannot alter a running router's verified bytes.

## Preparing a release (not deployment)

Authenticate `gh` normally, then run on supported native GNU/Linux or Apple
Silicon macOS:

```sh
python3 scripts/prepare-relay-release.py --version 0.0.3 --output /private/owned/catalog
```

The version is illustrative; it must match the binary running the relay. The
output's parent must already exist, belong to the preparing user and disallow
group/world writes. The output is created private (0700). Existing roots must
have the exact managed marker and safe permissions; unmanaged roots and existing
version directories are never overwritten. Run preparation under the same UID
that will load the catalog (or deliberately transfer ownership before startup).

Preparation downloads only SHA256SUMS, all three archives and install.sh from
`perapp/herdr-webapp` at `vVERSION`, into a unique private temporary directory.
It verifies the strict complete checksum allowlist before parsing archives.
Archives must contain exactly `herdr-webapp`, `pi-extension/commands.ts`,
`scripts/install-pi-extension.sh`, optionally `LICENSE`. No directories, links,
traversal, duplicates, sparse files, or extended metadata are accepted. Only the
raw binaries are published. Only the host-native binary is executed, once all
three archives pass validation, and its `--version` must match exactly.

An update installs a new immutable version directory, then atomically replaces
manifest.json under a process lock. Failure leaves the previous manifest intact;
a failed first publication may leave an empty managed root but never an active
partial manifest. Old versions remain on disk, but only the loaded manifest's
version is served. A running router deliberately does not hot-reload: parent
integration must explicitly load the new version when starting the matching
relay binary. Do not reuse a version for changed release bytes.

Offline tests (mock gh and tiny archives; no GitHub credentials/network):

```sh
python3 -B distribution/test_relay_catalog.py
cargo test relay_catalog   # after parent registers the module
```
