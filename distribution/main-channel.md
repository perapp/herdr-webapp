# Opt-in main channel

One HTTPS origin hosts stable at `/` and development main at `/main/`.
`/run` and `/main/run` launch the corresponding connector. `/main/_status`
reports sanitized availability and revision. No channel-specific Caddy route,
DNS record, certificate, webhook or permanent access key is needed.

## Hosted use

On a computer already running Herdr Core:

```sh
curl -fsSL https://herdr-webapp.perapp.dev/main/run | sh
```

Requires curl and Python 3.9+. Scan the terminal QR with your phone. The private
link grants full trusted-user shell access; do not share it. Main and stable
have separate identity/pairing state, browser preferences and connector caches.
Stopping a connector does not stop Core or its terminals.

The CI catalog provides GNU/Linux x86-64, GNU/Linux ARM64 and Apple Silicon macOS
connectors. Each binary is built and tested on its target architecture before
publication. Check the workflow's runner OS for system-library compatibility;
a GNU target name alone does not promise support for every older glibc system.
Unsupported targets fail explicitly, never silently substitute stable.

## Self-hosting

Normal startup does not poll, create main state or spawn an updater. To opt in:

```sh
./run.sh serve-relay --public-origin https://relay.example.com \
  --release-dir /srv/herdr/stable-catalog --enable-main \
  --main-source ci --main-state-dir /srv/herdr/main \
  --main-artifact-base-url https://perapp.github.io/herdr-webapp \
  --main-update-interval 5m
```

`ci` is the default main source. Preserve existing bind/port/TURN configuration.
`--public-origin` is an authority-only origin, never a URL containing `/main`.
The main child receives the public origin, channel base and TURN file path,
listens only on a private owner-only Unix socket, and cannot recursively enable
another updater. The TURN secret is never an argument value.

The entry executable embeds the Python manager. Publishing a new main child
**does not update that manager**. Already-deployed managers and connector launchers
identify candidates through legacy CLI `--version`, so that output must remain
the numeric package identity. New managers prefer explicit `--package-version`
and fall back only when that flag is unsupported. CLI `--build-version` reports
the human tag or timestamp/commit identity instead.

Likewise `/api/mode.version` is the Cargo package identity used by readiness
checks; `build.version` is the human-facing identity. Never substitute display
identities into these machine contracts. The `573df15`/`27a2009` display-version
rollout violated those contracts; the compatibility hotfix preserves old entries
without a production restart. Publish a fresh corrected revision rather than
deleting live locks or activation/failure state; rejected revisions remain
quarantined by their three-failure ledger.

CI-backed runtime needs
**Python 3.11+ and system CA certificates**, not Git, Rust, Cargo, Node, npm, Make,
`gh`, an SSH key or a GitHub token. Use a private writable persistent state volume
owned by the service UID, with a short container path such as `/main-state`.
The stable release catalog and TURN secret can remain mounted read-only.
The container root can remain read-only.

## CI publication and trust

`.github/workflows/main.yml` builds the exact main commit on standard GitHub
Linux and macOS runners. Expensive superseded builds are cancelled; publication
requires successful completion of the entire three-target set. Stable release
workflows, Git tags, release assets and stable `latest` are not modified.

GitHub Actions artifacts are intermediate job inputs, **not anonymous download
endpoints**. The publisher deploys an atomic static GitHub Pages site at
`https://perapp.github.io/herdr-webapp/`, containing:

- `main/publication.json`: repository, branch, workflow, run ID, source revision
  and SHA-256 of the exact manifest bytes.
- `main/client/manifest.json`: the complete schema-2 catalog.
- `main/client/<revision>/<target>/herdr-webapp`: raw SHA-addressed executables.
- `main/revisions.json`: a copy-forward manifest-digest ledger. Republishing an
  existing revision with different bytes fails closed; history is not evicted.

The descriptor identifies `perapp/herdr-webapp`, branch `main` and workflow
`.github/workflows/main.yml`. The configured repository is the expected publisher
identity; a custom publisher requires corresponding trusted configuration.
This is **trusted GitHub Pages HTTPS publication**, not independent signature or
cryptographic-attestation verification. Trust the repository, its Actions
publisher and the configured Pages origin with execution of downloaded code.
The public browser origin also supplies trusted JavaScript.

Intermediate artifacts have short retention and successful-run inputs are cleaned
up. Standard public-repository runners are used; no larger/premium runners or
paid cache-capacity increase is required. Artifact/cache storage and concurrency
still have limits. Pages holds the currently published complete set; an older
provider URL may expire after another deployment. The relay retains its own
verified rollback artifacts independently of CI artifact retention.

## Verification and activation

The manager polls the latest **successfully published complete** catalog, which
may lag the branch tip. It never builds source or falls back to compilation in CI
mode. Failed/cancelled/incomplete CI runs do not change the publication. An
unchanged revision is not downloaded or activated again.

Before activation the manager requires the exact three-target set, a consistent
source revision, bounded sizes, safe paths, strict JSON and matching checksums for
**all** binaries. Downloads stream into private staging with time/disk bounds;
URLs cannot supply credentials, redirects or arbitrary artifact destinations.
The native executable must report the advertised package version and compiled
Git revision. Foreign binaries are tested natively in CI, not executed by the
hosting server.

The candidate starts on a fresh private socket. Health, mode/channel/origin,
entry-point protocol, relative assets, API routing, catalog/native download bytes,
a real WebSocket challenge and wrong/duplicate Origin rejection are verified.
Only then is routing switched atomically. Existing main keeps serving throughout
acquisition or candidate failure; stable is independent. Old sockets drain for a
finite period. Registries are in memory: this is **not seamless migration**.
Clients reconnect without queuing/replaying ambiguous input or uploads.

Once loaded, the main frontend checks its source revision against `/main/api/mode`
on signaling reconnect and visible foreground return, without continuous polling.
An unchanged revision reconnects normally. A changed revision exposes an
**Update ready** action; reload is intentional, not an automatic side effect of
reconnect. Pairing, IME composition and uploads block reload; selected files and
unsent drafts require an explicit discard decision. Explicit reload may acknowledge discarding a draft,
not silently interrupt critical work. A per-target session guard bounds automatic
reload attempts if assets remain stale; storage/network failures do not force a
reload. Pairings/preferences survive, with no input replay or keyboard focus.
Already-open pre-feature tabs require one initial manual refresh.

Startup restores a verified cached artifact before contacting the network. A
legacy native-only local-build cache can be restored during migration, but every
new CI acquisition must contain all three platforms. Existing SHA bytes are never
overwritten with different data. Three revisions are retained, with draining
generations briefly pinned. Failed candidates preserve the current instance;
this does not promise automatic recovery from every corrupt cache or repeatedly
crashing active revision.

## Resources, rollback and disabling

Keep service/container CPU, memory, process and disk limits. Three complete
catalogs, in-memory snapshots and temporarily draining generations need more
space than one executable. Downloads enforce a two-GiB free-space reserve and
bounded file/aggregate sizes; these guards are not filesystem quotas. No unrelated
host data is pruned. A lean runtime no longer needs compilation-sized allowances.

For rollback, stop main tracking and restore an intact retained artifact using
an operator-controlled activation procedure; do not overwrite immutable binaries
or delete a live lock. Removing `--enable-main` stops the owned manager/children
and reserves `/main/` as unavailable, never as stable HTML. Stable catalog files
remain unchanged. Remove managed state only after the manager has stopped.

## Optional local-build source

`--main-source local` explicitly retains the previous trusted-source build mode.
Only this mode uses `--main-repository`, `--main-branch` and
`--main-build-jobs`. Provision Python, Git, Rust 1.90+, Cargo, Node 22.12+, npm and
a native C toolchain, plus private writable build/cache space and compilation
resource limits. Make is needed for normal repository verification workflows.
Private sources need an operator-managed credential mechanism; never put tokens
in URLs or mount a broad OAuth token/forwarded SSH agent into the runtime.

Local mode executes trusted repository build scripts and produces only the
server's native platform. It is not a sandbox. It is never an automatic fallback
when CI downloads fail.

## Verification limits

Run `make check`, `make release`, `make smoke`, isolated multi-channel browser
acceptance and public CI/catalog/launcher checks. Never test by restarting a
user's Core or terminating their terminals. Phone-sized Chromium does not prove
physical iOS/Android IME behavior or cellular/cross-NAT connectivity.
Storage namespaces prevent accidental stable/main collisions, not malicious
same-origin JavaScript access. Keep private links/QRs, credentials, SDP and TURN
authentication out of reports and logs.
