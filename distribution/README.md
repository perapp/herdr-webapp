# GitHub release distribution

The repository stays **private**. Downloads and installation require GitHub CLI
(`gh auth login --hostname github.com`) with access to `perapp/herdr-webapp`.
The installer also requires POSIX sh and Python **3.9 or newer**; no package
manager, Python packages, curl/PAT arguments, or root privileges are used.

Latest stable release installer (GitHub CLI's `--output -` supports a single
matched asset; `install.sh` is a pattern, **not** the positional release tag):

```sh
gh release download --repo perapp/herdr-webapp --pattern install.sh --output - | sh
```

Exact version, after `v0.0.1` has been published:

```sh
gh release download v0.0.1 --repo perapp/herdr-webapp --pattern install.sh --output - |
  HERDR_WEB_VERSION=0.0.1 sh
```

For download failure propagation without relying on the shell's pipefail support,
download the installer to a new private temporary directory, run it only after
`gh release download` succeeds, and remove that directory afterward.

## Platform and archive contract

Only these targets are shipped:

- `x86_64-unknown-linux-gnu` (Ubuntu 22.04 / glibc 2.35 baseline)
- `aarch64-unknown-linux-gnu` (cross GCC/sysroot on Ubuntu 22.04)
- `aarch64-apple-darwin` (Apple Silicon macOS)

No Windows, Intel Mac, or musl/Alpine build is claimed. A Rust cross build is not
a native runtime test; physical target verification is separate. macOS executable
assets are not Developer ID signed/notarized by this workflow.

Each archive is named exactly
`herdr-webapp-v<version>-<target>.tar.gz`, containing these regular files at the
archive root (no top-level container folder):

- `herdr-webapp` (executable, with frontend embedded)
- `pi-extension/commands.ts`
- `scripts/install-pi-extension.sh` (optional, **never** run automatically)
- `LICENSE` if present in the source tree

`SHA256SUMS` is a standalone release asset covering all three archives and
standalone `install.sh`. The package script accepts an already-built binary:

```sh
sh scripts/package-release.sh TARGET [BINARY [OUTPUT_DIRECTORY]]
```

It reads `[package].version` from `Cargo.toml`, validates `RELEASE_TAG`, GitHub tag
context, and any `HERDR_WEB_VERSION`/`RELEASE_VERSION`, checks the binary's
`--version` on its host target, and produces deterministic tar/gzip metadata.
Cross binaries are checked when installed.

## Installation / backend updater contract

Default installation root: `${XDG_DATA_HOME:-$HOME/.local/share}/herdr-webapp`.
CLI symlink: `$HOME/.local/bin/herdr-webapp` pointing to the root executable.
Absolute, normalized, nonsymlinked `HERDR_WEB_INSTALL_DIR` and
`HERDR_WEB_BIN_DIR` override these paths (useful for isolated verification).
The managed root is private (0700); the receipt is private (0600). The existing
installation parent must be owned by the current user and not group/world
writable, matching the backend updater's safety checks.

The exact root `install.json` schema is:

```json
{"schemaVersion":1,"repository":"perapp/herdr-webapp","target":"<target>","version":"<version>"}
```

The installer and web updater both coordinate through the sibling directory
`<install_dir>.update-lock`, acquired using atomic mkdir. Existing locks are
never removed by the installer. Only its own lock and randomly named private
staging directories are cleaned. The sibling `<install_dir>.pid` contains a
positive decimal PID; a live process (or inaccessible PID) blocks installation
with instructions to use the UI updater or stop **only** the web server.

The installer verifies the checksum and a strict archive allowlist before
extracting, and checks the new executable's exact `--version` before swapping.
Staging/backup stay on the installation parent's filesystem. Failed directory or
CLI swaps restore the previous managed installation. Unmanaged roots, symlinks,
and CLI files are not overwritten. If the OS prevents rollback, the backup is
retained and its recovery path is reported. Config/auth under the separate state
home are untouched. No core install/restart or terminal operation is performed.

## Publication and verification

`.github/workflows/release.yml` triggers only on pushed `v*` tags or manual
`workflow_dispatch` with an **existing** stable tag. The tag must match the crate
version and checked-out commit. Builds use that verified commit, npm's lockfile,
and `cargo --locked`; frontend and Rust host checks run in the matrix. The Linux
ARM artifact is cross-built rather than falsely claiming ARM runtime tests.

Only the publish job gets `contents: write`. It assembles the full manifest,
creates/resumes a **draft**, uploads all assets, checks exact remote names/sizes,
redownloads and verifies bytes, then publishes/marks latest. Published releases
are not modified by reruns. Failed uploads/verifications leave the draft hidden.

Offline tests (all generated fixtures and installs use temporary directories):

```sh
python3 distribution/test_release.py
```

These tests do not replace an authenticated installer run against a real,
published release, nor real ARM/macOS runtime verification. No release is created
by the local test suite or package script.
