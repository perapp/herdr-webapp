# Opt-in main channel

One public HTTPS origin can host two independent relay channels:

| URL | Channel |
| --- | --- |
| `/`, `/run`, `/client/…` | Installed stable entry point and approved release catalog |
| `/main/`, `/main/run`, `/main/client/…` | Last verified locally built main revision |
| `/main/_status` | Sanitized build/availability status |

No channel-specific Caddy route, DNS record, certificate, webhook or webhook
secret is required. Forward the whole hostname to the same entry point.
`--public-origin` remains exactly `https://host[:port]`, never a URL with a path.

## Start

Normal startup does **not** create main state, poll Git, build source or start a
main child. `/main` is reserved and redirects to `/main/`; disabled main returns
an explicit unavailable response, never stable HTML.

```sh
./run.sh serve-relay --public-origin https://relay.example.com \
  --release-dir /srv/herdr/stable-catalog

./run.sh serve-relay --public-origin https://relay.example.com \
  --release-dir /srv/herdr/stable-catalog --enable-main \
  --main-state-dir /srv/herdr/main --main-build-jobs 1 \
  --main-update-interval 5m
```

Keep existing `--bind`, `--port`, `--turn-url`, `--turn-secret-file` and
`--turn-ttl-seconds` configuration. The private main child inherits the canonical
public origin and TURN settings, not the public listener. It cannot recursively
enable another updater. The TURN secret stays in its private file, not argv.

The source defaults to `https://github.com/perapp/herdr-webapp.git`, branch
`main`. `--main-repository` and `--main-branch` are trusted operator configuration;
HTTP requests cannot select source code. Use existing Git credential helpers or
a dedicated read-only repository deploy key. For SSH credentials with an HTTPS
configured source, a private Git `url.…insteadOf` rule can select SSH. Verify
host keys; do not disable TLS/SSH verification, embed tokens in URLs, persist a
forwarded agent, or copy a broad personal OAuth token into the runtime.

## Build environment and resources

Opt-in operation requires **Python 3.11+, Git, Rust 1.90+, Cargo, Node.js 22+,
npm, and a native C compiler/linker**. Make is needed for the repository's normal
`make release`/verification workflow. The supervisor performs the same frontend
before embedded-Rust ordering using locked npm/Cargo installation. No GitHub
Actions workflow is started by polling or by ordinary pushes to main.

The current binary-only runtime container is not a build environment. Add build
tools and a persistent private writable volume; keep the stable catalog mounted
read-only. Run source/build state as the service UID, not world-writable storage.
Use a short state directory: private Unix socket paths must fit within 100 bytes.
The default state is `$XDG_STATE_HOME/herdr-webapp/main` or
`~/.local/state/herdr-webapp/main`.

Allow several GiB for the toolchain, dependency downloads and release build.
A two-GiB free-space reserve is required before/during builds. Three successful
SHA artifacts are retained, with draining generations temporarily pinned. Build
workspaces/caches are disposable managed state; this is not a filesystem quota.
Use filesystem quotas and service/container CPU, memory, swap, process and disk
limits. On a small host, start with **one build job**, CPU 200%, memory 3 GiB and
bounded swap, after checking capacity. Never reclaim unrelated data automatically.
Use `KillMode=control-group` or container process isolation for abnormal-shutdown
cleanup. Resource exhaustion can still affect stable; process separation is not
resource isolation.

**Building main executes trusted repository code**, including npm and Rust build
scripts. It is not a sandbox. Main and stable share one browser origin: separate
storage names prevent accidental collisions, not access by malicious same-origin
JavaScript. Enable this only for a source repository you trust with that origin.

## Polling, activation and recovery

At startup the manager restores a previously verified artifact independently of
network availability, then checks the configured branch. Polling defaults to
five minutes with bounded retry/backoff. Unchanged active revisions do not build.
Failed revisions are remembered; rapid updates are serialized, not built in
parallel. Force-updated branch tips are identities, not semver-ordered releases.

Each candidate uses an exact Git SHA, builds frontend then Rust, and embeds that
SHA. The manager verifies provenance, main channel/base/origin/protocol identity,
HTML/assets, API, catalog/download bytes and WebSocket behavior on a private
owner-only Unix socket. It atomically records activation only after verification.
The old main continues serving while a candidate builds or fails. Child crashes
have bounded restart backoff. Main failures never intentionally stop stable.

The public entry streams requests and WebSocket upgrades to the fixed private
endpoint, without buffering terminal traffic or trusting forwarding headers.
The private main backend uses a shared admission bucket; it does not infer a
client identity from untrusted `X-Forwarded-For`.

Old signaling processes drain for a finite period before stopping. Registries
are in memory: activation is **not seamless session migration**. Connector and
browser reconnect create fresh sessions; ambiguous input/uploads are not queued
or replayed, and reconnect must not open the phone keyboard without a gesture.

For rollback, stop main tracking before changing managed activation records and
restore an intact retained artifact; do not edit or overwrite immutable binaries.
Disabling `--enable-main` stops the owned manager/children and reserves `/main/`
as unavailable. Stable release files remain untouched. Remove managed main state
only after the manager has stopped; never delete a live lock to bypass ownership.

## Matching connectors

Run the main launcher at **`https://relay.example.com/main/run`**. It uses a
separate endpoint-keyed supervisor cache and pairing state. Stable launcher state
and existing pairings remain at their original locations. Browser identity/UI
storage and native signaling transcripts are channel-specific too.

Main manifests use schema 2, package version plus full Git revision, and
immutable `/main/client/<sha>/<target>/herdr-webapp` URLs. Native executable and
readiness provenance must match the advertised revision. Revisions may change
without a package-version bump. Stable schema-1 three-platform catalogs and
immutable version URLs are unchanged; main never overwrites them.

**Only the server's supported native platform is built automatically.** A Linux
x86-64 server does not produce Apple Silicon or Linux ARM64 builds. The launcher
explicitly rejects unavailable targets and lists those available; it never
silently substitutes a stable connector. Native GNU/Linux target naming also
does not guarantee compatibility with older glibc versions than the build host.
Cross-platform import/build production is future work, not faked support.
A developer on another supported platform can build the same trusted revision
locally and run its connector directly (without the download supervisor):

```sh
# In a clean trusted checkout at the desired main revision:
HERDR_BUILD_REVISION="$(git rev-parse HEAD)" make release
./target/release/herdr-webapp connect-relay \
  --relay-url https://relay.example.com/main
```

## Verification limits

Run `make check`, `make release`, `make smoke`, plus isolated dual-channel browser
acceptance. Local Git fixtures cover polling/build/activation failure paths;
actual desktop and phone-sized browser tests cover input/reconnect. These do not
prove behavior on physical iOS/Android keyboards or actual cellular/cross-NAT
networks. Never test by restarting a user's Core or terminating user terminals.
Keep private pairing links/QRs, credentials, TURN authentication and SDP out of
reports and logs.
