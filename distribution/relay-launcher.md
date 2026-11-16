# Per-relay launcher and Core bootstrap

Public entry point: `curl -fsSL https://<relay>/run | sh`.
Requires Python **3.9+**, curl, and a stable target: GNU Linux
x86-64/arm64/armv7/i686, musl Linux x86-64/arm64, or Intel/Apple Silicon macOS.
Detection uses Python pointer width to select 32-bit userspace on a 64-bit kernel;
unknown libc is rejected unless a musl loader is detected. Windows is unsupported.
Older stable releases still provide only GNU Linux x86-64/arm64 and Apple Silicon
macOS. MAIN remains limited to those original three; broader stable detection
does not imply MAIN availability.
The connector remains a per-relay cached executable. When local Core is missing,
the hosted launcher and release installer now install **stable, generation-1 Core**
through its official installer; selecting `/main/run` does not select a development
Core. Starting the standalone `herdr-webapp` executable never installs dependencies.

## Core dependency setup

Both hosted endpoints embed `distribution/run-relay.sh` and the single-file
`relay-launcher.py` via the actual relay handlers in `src/relay_server.rs`; the
source checkout's `run.sh` is not this entrypoint.

- First probe the selected local socket with a bounded generation-1/framed `hello`.
  A healthy running Core is reused without needing any executable on PATH.
- Otherwise discover `herdr-core`/Herdr's `hc` on PATH and usual official,
  Homebrew, Nix, mise and Cargo paths. An unrelated `hc` is not Herdr Core.
  Existing/unusable Core installations are never replaced. The discovered folder
  is prepended to the current process PATH, including for subsequent child swaps.
- Only if missing, fetch `https://releases.herdr.dev/core/latest.json`, require
  generation **1**, and pin its stable version using `HC_VERSION` when running
  the downloaded official `/core/install.sh`. `HERDR_RELEASES_URL` may select an
  HTTPS mirror; credentials, query and fragments in its root are rejected.
  Manifest/installer downloads are bounded; the official installer verifies its
  release payload/checksum and macOS signing. The official installer owns platform
  installation and bundle details, not another binary replacement implementation.
- Installation goes to `HC_INSTALL_DIR` or `~/.local/bin`. A private lock directory
  serializes these dependency installs. An existing target (including a dangling
  symlink) is refused before calling the official installer, because its update
  branch can restart Core. The lock does not coordinate unrelated external tools:
  do not run another Core installer concurrently. There is no sudo/escalation.
- The launcher calls the supported, idempotent `herdr-core server start --json`,
  then validates the socket handshake before starting the connector. It never
  calls `server restart`, `server stop`, or `hc update`. The release installer
  also prepares and starts a compatible Core when needed, so the installed webapp
  can connect immediately. Existing compatible Cores are reused without restart.
- Incompatibility is a failure with an explicit update instruction: required
  generation 1 with framed streams. For managed binaries use `brew upgrade
  herdr-core`, `mise upgrade herdr-core`, or update the Nix configuration, not the
  official installer over a managed executable. Review compatibility before any
  explicit Core update. No connector or frontend upgrade ever upgrades Core.

Managed/custom endpoint setup:

```sh
curl -fsSL https://herdr-webapp.perapp.dev/run | sh -s -- --no-install-core
curl -fsSL https://herdr-webapp.perapp.dev/main/run | sh -s -- --core-socket /path/to/core.sock
# The private GitHub release installer accepts the same opt-out:
gh release download --repo perapp/herdr-webapp --pattern install.sh --output - | sh -s -- --no-install-core
```

`HC_SOCKET`/`--core-socket` and socket settings in the Core configuration are
explicit endpoints: an unavailable one does **not** trigger a local install or
start. The connector reports connection failure normally. `HC_SESSION`/`--session`
is preserved for default local lifecycle selection. Remote Core must be exposed
through a supported local Unix-socket route (for example a configured tunnel);
this launcher does not invent TCP/SSH Core transport. `--no-install-core` skips
all bootstrap probing/installation/start; the connector still validates Core.

Unsupported platforms fail before dependency installation. On network, permission,
verification or interrupted-install failure, the command fails rather than claiming
the connector is ready. Fix connectivity/disk/permissions and rerun to reuse a
verified existing Core. SIGINT/SIGTERM reap installer subprocesses through the
launcher process group normally; after an uncatchable interruption inspect the
install destination and remove `.herdr-web-core-install.lock` **only after ensuring
no installer remains active**. A partial/unusable existing Core requires explicit
repair; it is not silently overwritten. Installer subprocesses use no stdin, so
curl-piped script bytes cannot be consumed as prompts.

`distribution/core-bootstrap.py` is the maintained source of the dependency block;
its exact copies in the standalone Python launcher and self-contained shell release
installer are checked by `test_core_bootstrap.py`. Keep all three copies in sync.

## Server contract

- `/run`: serve `run-relay.sh`, replacing **every** `__HERDR_RELAY_ORIGIN__`
  with the validated, canonical public HTTPS origin. Validate **before** string
  substitution: ASCII DNS name or bracketed IP, optional numeric port, no userinfo,
  quotes, backslashes, shell syntax, whitespace, path, query or fragment. Do not
  derive it from an untrusted Host/forwarded header. The shell's later validation
  is defense in depth, not a defense against unsafe template substitution.
- `/client/launcher.py`: serve `relay-launcher.py`. HTTPS is the bootstrap trust
  anchor. Do not redirect either endpoint; do not include access tokens.
- `/client/manifest.json`: JSON (at most 128 KiB):

  ```json
  {"schemaVersion":1,"protocolVersion":1,"version":"0.0.5","assets":[
    {"target":"x86_64-unknown-linux-gnu",
     "url":"/client/0.0.5/x86_64-unknown-linux-gnu/herdr-webapp",
     "sha256":"<64 lowercase hexadecimal characters>","size":12345678}
  ]}
  ```

  Stable catalogs publish exactly the historical three targets or all eight
  non-Windows targets in `distribution/release-targets.json`; incomplete cohorts
  are rejected by preparation and relay loading. MAIN has its separate original
  three-target catalog. Artifacts are **raw executables**, not archives.
  Exactly one matching target, stable `x.y.z` versions, protocol/schema exactly
  integer `1`, size 1..256 MiB. Paths must start `/client/`, contain only ASCII
  alphanumerics and `._/-`, with no `..` components. No redirects or cross-origin
  downloads. HTTPS plus the manifest checksum authenticates downloads; this is
  not an independent signature or protection against a compromised relay.
- Artifacts must be immutable at a given version. Publish verified artifacts
  before advertising them. The launcher fails closed on cached hash mismatch.

## Connector contract

A verified executable's `--version` must exit zero and print exactly
`herdr-webapp <version>` (trailing whitespace allowed, bounded output).
Then the supervisor launches, inheriting terminal stdio:

```
<cached-executable> connect-relay --relay-url <canonical-origin> \
  --state-dir <private-persistent-directory> --ready-file <unique-private-path>
```

No token is passed on argv. The connector stores pairing material in `state-dir`
and gracefully exits on SIGTERM. It must atomically replace `ready-file` with a
regular, single-link, user-owned **0600** JSON file only after healthy connection:

```json
{"schemaVersion":1,"pid":123,"version":"0.0.5","protocolVersion":1,"ready":true}
```

The PID must equal the directly launched child. Readiness is not a durable PID
file or permission to kill a process; only live `Popen` children are signalled.
Readiness files are removed after checking. Healthy startup defaults to 20s.

## Lifecycle and local files

Cache: `${XDG_CACHE_HOME:-~/.cache}/herdr-webapp/relay/<origin-hash>/`.
Pairing state: `${XDG_STATE_HOME:-~/.local/state}/herdr-webapp/relay/<origin-hash>/`.
Both are private 0700 directories; files must be user-owned, single-link and
private. Symlink directory components are rejected. A nonblocking file lock in
state serializes supervisors for the same canonical origin, even with different
cache roots. Nothing is added to PATH, shell profiles, autostart, or global config.
Stopping the launcher reaps its child, but keeps pairing state and verified cache.
Deleting cache does not delete state. Explicitly deleting state forgets pairing.
The origin is never a secret URL: origins with credentials/query/path are rejected.

Manifest checks default to **30 seconds**, with **check-only** behavior: a newer
connector is offered but not installed without an intentional update request.
`--auto-update` explicitly opts into the older automatic-apply behavior (connector
only, never Core). A fresh launcher invocation still selects the latest compatible
release as before. Downloads and `--version` probes happen while
the old connector remains running. Newer compatible versions only; a readiness
failure stops that exact child and restarts the previous verified version using
the same state. A failed version is suppressed for the supervisor's lifetime;
a fresh invocation retries it. A fresh invocation also falls back to the last
healthy cached version if discovery/download/startup fails; an explicitly
incompatible manifest schema/protocol blocks a fresh launch instead. Cached fallback does
not imply the relay is online: readiness still must succeed. At most current and
previous executable are retained after a transition (a download temporarily adds
one); restarts may trim to just current. Pairing files are never pruned.

Interrupt/termination signals are forwarded through cleanup; normal child exit
status or terminating signal is propagated. No daemonization or persistent job is
created. The shell restores `/dev/tty` stdin when available, otherwise `/dev/null`.
This preserves terminal input in ordinary `curl | sh` use, but cannot promise
terminal job-control behavior for every shell/embedding. SIGKILL and machine
crashes cannot be cleaned up synchronously; abandoned download/save temporaries
are removed under the lock next run. Same-user hostile code and writable ancestors
raced by the same user are outside the file-ownership protection boundary.

Browser public keys and the reusable private capability commit atomically in
`pairings.json`; `pairing.json` is a derived display fixture. Explicit policy markers
distinguish persistent capability/session data from legacy expiring state, while
legacy outer storage versions stay readable by installed rollback binaries. Upgrade
preserves trusted keys and replaces a legacy expiring capability once. Downgrading
runs that older version’s expiry behavior; upgrading again preserves only currently
trusted state. Never restore old authorization backups after rotation, which would
undo revocation. Readiness validates initial health only, not ongoing application
behavior. An already-ready connector that subsequently exits ends the supervisor;
it is not endlessly restarted.

Development only: invoke Python directly with
`--relay-url http://127.0.0.1:<port> --allow-http-loopback`. The public shell never
allows HTTP. Optional `--cache-dir`/`--state-dir` are private root overrides;
`--poll-interval` 0.1..3600, `--ready-timeout` 0.1..120, and
`--network-timeout` 0.1..60 are bounded seconds (sub-second values are for tests).
Network requests have socket timeouts and a total streaming bound of four times
network timeout, checked between bounded reads. Environment proxies are ignored.

## Managed connector control API (frozen v3)

Only managed connector children inherit `HERDR_CONNECTOR_CONTROL_SOCKET`, pointing
to a supervisor-owned Unix socket in a unique private 0700 temporary directory;
the socket is 0600 and survives connector replacement. It is removed on supervisor
shutdown. It is not a browser parameter or a relay endpoint. The backend must use
only its inherited setting, perform its normal authenticated computer-scoped
checks, and never accept a socket path, executable path, URL, or command from UI
input. Same-user local code is inside the trusted-user boundary.

One JSON line per connection, at most 16 KiB; 2-second socket timeout. Requests:

```json
{"schemaVersion":1,"action":"status"}
{"schemaVersion":1,"action":"check"}
{"schemaVersion":1,"action":"update","expectedVersion":"0.0.5"}
```

Main updates also require `expectedRevision` (the full checked commit SHA).
`update` compares both fields against the **last successfully checked candidate**;
there is no catalog refetch selecting a different version at apply time. Stale
requests return `failed` with a retry/check message. Mutations serialize; duplicate
requests return the in-progress status. Status remains responsive during network
checks/downloads. Downloads, checksums, numeric package-version/revision probes,
readiness checks and rollback are identical for intentional and opt-in automatic
updates. A failed check invalidates the candidate. Backend translates this fixed
IPC into its HTTP component status/identity schema.

Responses include `schemaVersion:1`, `status` (`idle`, `checking`, `current`,
`available`, `failed`, `updating`), `currentVersion`, nullable `availableVersion`,
`channel` (`stable`/`main`), and nullable `message`. Main adds `currentRevision` and
nullable `availableRevision`. `idle` means no completed network check; do not show
“up to date” until `current`. Current version is the child's human `ready.version`;
main's offered version is its numeric package identity plus full available revision
for compare-and-swap. Do not present that package number as a tagged main release;
if timestamp metadata is unavailable, display “Main build <short SHA>” instead.
The socket and any associated internal paths must never be exposed to the browser.

Offline verification (isolated fixtures, no public downloads or live Core):

```
python3 -B distribution/test_core_bootstrap.py
python3 -B distribution/test_relay_launcher.py
python3 -B distribution/test_release.py
```

Tests use isolated temporary files, a loopback fixture relay, and real tiny child
executables; no live core, public release, browser, or persistent server is used.
