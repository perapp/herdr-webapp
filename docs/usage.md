# Herdr Web — usage and development

Commands in this guide assume the repository root unless stated otherwise.

The native-phone-app experience of **Herdr**, delivered through a browser.
Packaged as one Rust executable with the
Svelte/TypeScript/xterm.js frontend embedded. Core remains the terminal authority;
the browser shares the desktop's machine-owned spaces, tabs and panes. The desktop
application does not need to be running.

## Hosted access / outbound connector

The same executable also provides a hosted frontend/signaling service and an
outbound laptop connector. Local/LAN access remains unchanged (no subcommand, or
`herdr-webapp local`). The public service is available at
**https://herdr-webapp.perapp.dev** (stable) and
**https://herdr-webapp.perapp.dev/main/** (main branch).
Any independently hosted HTTPS origin is also supported.

Once the relay is deployed with an approved release catalog:

```sh
curl -fsSL https://herdr-webapp.perapp.dev/run | sh
```

Run this on the computer already running Herdr Core. It requires curl and Python
3.9+, but no GitHub login or permanent installation. The launcher verifies the
public native executable, keeps a private per-origin cache/state, and supervises
`connect-relay`. Scan its terminal QR using your phone camera and open the link.
The QR pins the laptop identity and carries an expiring, one-use pairing secret;
it never points to the laptop's changing IP. Paired browser keys are nonextractable
P-256 keys persisted in IndexedDB. Pairing grants full trusted-user shell access.
The running connector automatically renews the link and QR after five minutes
or successful pairing; existing trusted browsers and active terminals are unchanged.
In-progress pairing is protected by a bounded reservation. The pinned terminal card
updates in place; refreshed credentials are not appended to captured logs.
`connect-relay --reset-pairings` explicitly forgets paired browser
keys on startup. Deleting browser storage also requires pairing again.

Terminal operations, input, output and uploads use encrypted WebRTC DataChannels,
directly when possible and through separately configured coturn when necessary.
The hosted service routes signaling, not terminal plaintext. The hosting origin
is still trusted to deliver JavaScript and approved downloads; this does not
protect against a compromised frontend publisher. No new exclusive-control lock
is added. Lost connections discard pending input/operations instead of replaying.
TURN credentials are short-lived; renewal currently makes a brief authenticated
reconnection, not in-place ICE renegotiation. Direct sessions do not periodically
reconnect. Physical iOS/Android keyboards require separate device validation.

The supervisor checks compatible newer versions while the connector runs, verifies
size/SHA-256/`--version`, and performs a readiness-gated restart with rollback if
startup fails. Laptop identity and pairings persist; Core and its terminals are
never restarted. This is **not** zero-interruption updating or continuous
end-to-end health rollback. Stopping the launcher stops its connector, not Core.

The interactive connection card generates a crisp **PNG QR** and displays it inline
when an attached terminal positively answers a Kitty graphics query (including
Herdr App). Terminal names/environment variables are not treated as proof. The
card owns a bounded input reader while active, restores terminal modes on normal
exit, and retains the complete private link plus a text QR fallback. Plain or
non-TTY output never probes or emits image controls. Text QR fallback reports its
actual row/column requirement rather than asking vaguely for a bigger window.
The connector holds PNG data in memory without saving image files; private URLs
and QR images remain terminal output, never public downloadable assets.

Manual connector (already downloaded executable):

```sh
herdr-webapp connect-relay --relay-url https://your-relay.example
```

Host the relay behind an HTTPS reverse proxy, without exposing a Core socket:

```sh
python3 scripts/prepare-relay-release.py --version 0.0.8 --output /srv/herdr/catalog
herdr-webapp serve-relay --bind 0.0.0.0 --port 8080 \
  --public-origin https://your-relay.example --release-dir /srv/herdr/catalog \
  --turn-secret-file /private/turn-secret \
  --turn-url 'turn:turn.your-relay.example:3478?transport=udp' \
  --turn-url 'turn:turn.your-relay.example:3478?transport=tcp'
```

Preparation requires a published matching GitHub release and authenticated
`gh` on the preparing host only. The catalog and binary must belong to the relay
UID, with no group/world-writable paths. The shared TURN secret must be a private
0600 file and never a command-line value. Without a catalog, pairing/signaling can
run but public executable downloads return 503; without TURN, only direct paths
work. The checksum-provenanced local ICE patch supports native TURN **UDP, TCP,
and certificate-verified TLS**. Real isolated coturn acceptance forces both peers
through each transport; see `RELAY-VERIFICATION.md`. TCP/TLS carries UDP relay
allocations, not ICE-TCP. TLS uses system roots and hostname/IP-SAN verification;
configure a `turns:turn.your-relay.example:5349?transport=tcp` URL only with a valid
certificate and reachable listener. HTTP proxies and TURN-over-DTLS are not added.
Public network/TURN availability is not implied by an HTTP health check.

See [`RELAY-PROTOCOL.md`](../RELAY-PROTOCOL.md),
[`distribution/relay-launcher.md`](../distribution/relay-launcher.md), and
[`distribution/relay-catalog.md`](../distribution/relay-catalog.md) for wire, private
state, publication, update/rollback and trust contracts. Development-only plain
HTTP requires `--allow-http-loopback` on both relay and connector with a loopback
origin/bind; production requires HTTPS. No edge, DNS or router changes are made by
these commands automatically.

## Use the main branch

Main is the **unreleased development channel**. On a computer already running
Herdr Core, start its hosted connector with:

```sh
curl -fsSL https://herdr-webapp.perapp.dev/main/run | sh
```

This requires curl and Python 3.9+, but no GitHub login. Scan the terminal QR
with your phone and open its private link, which uses
**https://herdr-webapp.perapp.dev/main/**. Treat the QR/link as full shell access.
Main has separate pairing, browser preferences and connector cache/state from
stable; pair once for each channel. Core and its terminals are not restarted.
To return to stable, stop the main launcher and run the `/run` command above.

GitHub Actions builds and tests all three native platforms on main pushes.
The hosted service checks the latest complete successful publication every five
minutes, keeping the last usable main build serving during CI or download
failures. Activation can briefly interrupt connections. The launcher verifies
and follows the matching connector revision automatically.

The main browser checks for a changed revision on reconnect and when returning
to the foreground. It reloads automatically when safe, not on ordinary network
interruptions. Pending pairing, input composition, uploads or unsent drafts defer
the reload and show **Update ready**. Finish pending work, or explicitly reload
with the displayed draft-discard warning. Pairing/preferences remain stored;
input and uploads are never replayed and the phone keyboard is not opened.
Tabs opened before this feature need one manual refresh to install it.

Hosted main supports GNU/Linux x86-64 (Ubuntu 22.04 / glibc 2.35+), GNU/Linux
ARM64 (Ubuntu 24.04 / glibc 2.39+) and Apple Silicon macOS (built/tested on the
standard macOS 15 ARM runner). There is no silent fallback to stable. Stable
Linux release builds retain their Ubuntu 22.04 / glibc 2.35 baseline.

### Build a main connector locally (including macOS ARM64 / Linux ARM64)

For a custom build, or a separately hosted native-only local-build channel,
build the revision currently served by the main endpoint. Install Git, curl, Python 3.9+, Rust 1.90+, Node.js 22.12+
(or 24+), npm, Make and a native C compiler first. In a **new checkout**:

```sh
git clone https://github.com/perapp/herdr-webapp.git
cd herdr-webapp
revision=$(curl -fsSL https://herdr-webapp.perapp.dev/main/api/mode | \
  python3 -c 'import json,sys; print(json.load(sys.stdin)["revision"])')
git fetch origin "$revision"
git checkout --detach "$revision"
HERDR_BUILD_REVISION="$revision" make release
./target/release/herdr-webapp connect-relay \
  --relay-url https://herdr-webapp.perapp.dev/main
```

Scan this connector's QR to pair. A directly run source build does **not** have
the download supervisor's automatic updates; rebuild the newly served revision
and restart only the connector when you want to update. Do not stop Core.

### Host your own stable + main service

`serve-relay --enable-main` keeps stable available while polling verified CI
catalogs for **`/main/`** and **`/main/run`**. The default `--main-source ci`
uses public GitHub Pages publication: no GitHub login/token, extra hostname,
certificate or Caddy route is needed. Without `--enable-main`, no updater starts.

Use a private persistent `--main-state-dir` and `--main-update-interval 5m`.
Runtime needs Python 3.11+ and CA certificates, not a build toolchain. Downloads
are immutable by Git SHA; all three targets must verify before activation.
`--main-repository` identifies the expected HTTPS github.com OWNER/REPO publisher,
and `--main-branch` must be `main` in CI mode. Explicit `--main-source local`
retains trusted native-only builds for custom deployments; it is never an
automatic fallback. Stable catalogs and existing client state remain separate.
Main JavaScript shares stable's browser origin, so trust it with that origin.

See [main channel operations](../distribution/main-channel.md) for publication
trust, optional local builds, resource limits, lifecycle and rollback.

## Install a release

[**Download the installer**](https://github.com/perapp/herdr-webapp/releases/latest/download/install.sh)
· [Release assets and versions](https://github.com/perapp/herdr-webapp/releases)

This repository is public. The release installer currently uses authenticated
GitHub CLI (`gh auth login`); no special repository access is needed. Installation
also requires Python 3.9+ (the running local/LAN web server does not). Then
download and run the installer:

```sh
installer_dir=$(mktemp -d)
gh release download --repo perapp/herdr-webapp --pattern install.sh --dir "$installer_dir" &&
  sh "$installer_dir/install.sh"
rm -rf "$installer_dir"
herdr-webapp --port 8001
```

The installer selects Linux x86-64/ARM64 or macOS ARM64, verifies the release's
SHA-256 checksum and executable version, and installs per user under
`${XDG_DATA_HOME:-~/.local/share}/herdr-webapp`, with a CLI symlink in
`~/.local/bin`. Ensure that directory is on `PATH`. Linux GNU builds target the
Ubuntu 22.04 / glibc 2.35 baseline. Windows and Intel macOS are not supported.
A compatible running `herdr-core` is still required; installing or updating this
web app does not install, stop or update Core.

Use `HERDR_WEB_VERSION=0.0.1` to select a specific release. Installation paths can
be overridden with absolute `HERDR_WEB_INSTALL_DIR` and `HERDR_WEB_BIN_DIR` paths.
Existing unmanaged files are never replaced. Stop only the web server before
re-running the installer on a running managed installation, or use its update UI.

## Updating local/LAN installations

The bottom of the burger menu shows the running server version and **Check for
updates**. Release checks run shortly after startup and hourly afterward. When a
new version is available, **Update…** opens a confirmation popup. **Update and
restart** downloads and verifies the matching GitHub release, stages it, then
restarts only the web server. Core and its terminals keep running. The browser
reconnects using its existing session and reloads the updated frontend.

Updates require `gh` authenticated on the server (or a server-side `GH_TOKEN`)
and `tar`. GitHub credentials are never sent to browsers. Updates are explicit,
not installed merely because a version is available. Source builds can report a
new version, but only installer-managed releases can self-update. Authentication,
configuration and browser preferences remain separate from installed binaries.
Checksums protect against corruption, not a compromised GitHub publisher; these
releases do not currently have an independent signature-verification scheme.

## Build from source

Build prerequisites: **Rust 1.90+**, **Node 22.12+ (or 24+)**, npm, Make, and a native C compiler. Runtime: a running
compatible `herdr-core` (tested with 0.1.19, protocol generation 1). No Node, Python,
frontend server or source checkout is needed by the built executable.

```sh
./run.sh                         # builds on first run, then runs the binary
# Or:
make release
./target/release/herdr-webapp
```

The default bind is **0.0.0.0**. Without an explicit port, the server chooses the
first available port from **8001–8100**, never displacing another listener. With
`--port`, an occupied port is an error. It prints an actual LAN URL.

Open that LAN URL from a phone, tablet, or another computer on the network.
“Same origin” means the web page and its API requests use the same scheme,
hostname/IP and port; it does **not** require the browser to run on the server.
No CORS exception is needed for a phone: requests use the address it opened.
Sign-in errors distinguish invalid tokens (401), origin rejection (403), and
rate limiting (429). Do not share the private QR or token when reporting an error.

**In a terminal, the connection QR and LAN URL stay fixed at the top, with a
bounded live log below.** Enlarge a small pane to see the full QR. Ctrl+C stops the
server and restores the terminal. Redirected output, `TERM=dumb`, and
`--no-dashboard` use plain output instead. **Both the displayed LAN URL and QR
embed the access token and sign in automatically.** The pinned URL wraps without
truncating the token and stays clickable in terminals with hyperlink support.
Treat both as private, full-shell-access credentials.

```sh
herdr-webapp --bind 0.0.0.0 --port 8001
herdr-webapp --core-socket /run/user/1000/herdr-core/default.sock
herdr-webapp --session development --port 9001
herdr-webapp --token-file /private/path/auth-token
herdr-webapp --public-origin https://herdr.example.org
```

Options also accept `HERDR_WEB_BIND`, `HERDR_WEB_PORT`, `HC_SOCKET`, `HC_SESSION`,
`HERDR_WEB_TOKEN_FILE`, and `HERDR_WEB_PUBLIC_ORIGIN`. Socket selection follows
explicit `--core-socket`/`HC_SOCKET`, core's `config.toml` socket setting, and then
the session's runtime/state directory. The webapp does not start, stop, update or
restart your core. Missing core connections are shown explicitly and retried.

`run.sh` reuses an existing release binary. After source changes run `make release`
or `HERDR_WEB_REBUILD=1 ./run.sh`. To distribute, copy just
`target/release/herdr-webapp` to a compatible Linux system and run it from any
folder. This first implementation uses Unix sockets; Windows is not supported.

## Security

**Signing in grants full trusted-user / shell access**, equivalent to controlling
Herdr.app. This is intentionally not a per-command sandbox or a multi-user system.

- A random 256-bit token is generated once at
  `${XDG_STATE_HOME:-~/.local/state}/herdr-webapp/auth-token` and reused. It must be
  a regular, user-owned, private file; symlinks and permissive modes are refused.
- Scan the QR, open the private LAN sign-in URL shown above the logs, or enter
  the token on the sign-in screen. The fragment is removed before the UI starts;
  it is not sent in HTTP request URLs or saved in local storage.
- Successful sign-in creates a random, opaque, HttpOnly, SameSite=Strict
  session cookie, valid for seven days. Sessions survive server restarts using a
  private session store beside the token file (token filename + `.sessions`).
  Only hashed session IDs are stored; rotating the access token revokes all
  sessions. A private `.sessions.lock` sidecar prevents concurrent writers; only
  one server can use a given token/session store at a time. Do not delete the
  lock file while a server is running.
  The logout API revokes that session, including open streams (within five seconds
  for terminals and fifteen seconds for state streams). Access tokens are not logged
  by API handlers. **Both interactive and plain launcher output contain the private URL and QR.**
- Keep the QR, token file and captured launcher output private. To revoke the
  access token and all sessions, stop this web server, remove its token file,
  and restart it. Existing in-memory sessions from older versions require one
  final sign-in after upgrading.
- All browser mutations and WebSocket upgrades require the exact same Origin.
  There is no permissive CORS, and neither `Forwarded` nor `X-Forwarded-Host` is
  trusted. The UI uses escaped text, a restrictive CSP, and no mod execution.
- **Default HTTP is unencrypted.** Use a trusted LAN/VPN, an SSH tunnel, or an
  HTTPS reverse proxy. Do not expose this directly to the Internet. For a proxy,
  set `--public-origin` to the exact external origin (no trailing slash); this
  also enables Secure cookies. Proxy WebSockets and SSE without buffering.

## Initial app

- Terminal-first chrome: one compact top bar with the current space/tab and a
  common burger drawer on desktop and mobile. No desktop sidebar or tab strip.
  Desktop shows the selected tab's full pane layout; mobile shows one pane.
  Secondary desktop commands live under the ellipsis menu.
- The burger drawer navigates
  computers → workspaces → tabs → panes, including inactive stack panes. Selecting
  a pane does not alter shared layouts. Private selection survives reload.
  An Agents tree below Spaces shows reporting agents and live descendants;
  selecting a mapped agent opens its exact pane. Unplaced children explicitly
  indicate that native terminal peek is unavailable on the web. There is no
  sign-out button. Clear site cookies to forget this browser's login, or rotate
  the access token to revoke all sessions. The logout API remains available.
- Settings stays pinned at the bottom of the drawer while Spaces/Agents scroll.
  Appearance offers System (default), Light and Dark, saved only in this browser.
  Changes update chrome and terminal colours immediately without reconnecting,
  and send native viewer FACTS without claiming focus or resize ownership.
  System changes are followed only in System mode. Terminal font size is
  adjustable from 8–28 px (default 13), with a slider, step buttons and reset.
  It applies immediately to all panes, refits through native resize semantics,
  and is saved only in this browser.
  **Version** opens a read-only popup for the loaded browser app, the connected
  webapp/connector, the serving relay, and the connected Herdr Core. Main/source
  builds include their reported commit and commit date/time, with an explicit
  uncommitted-changes indicator when applicable. The browser version identifies
  its loaded assets, not a possibly newer relay. Local mode marks the relay as
  unused; legacy or missing metadata is shown as unavailable, never inferred.
  Current Core reports its version but not its commit/date, and disconnected Core
  information is labeled last observed. Commit time is not build/deployment time.
- Real xterm terminals: raw framed attach output, keyboard/paste, focus and PTY
  resize. Byte credit is replenished only after xterm parses the output.
- A native-phone-app interaction model: terminals are **interactive immediately**.
  Tap the terminal to type; no Take/Release control workflow. On phones, the
  keyboard icon reveals accessory keys for Esc, Tab, Ctrl C and arrows,
  with an Attach image action on a second row. Reconnect automatically restores input
  without replaying disconnected keystrokes or opening the keyboard unprompted.
  The core arbitrates resize ownership; browser terminals follow its authoritative
  dimensions rather than fighting other clients. Explicit observer attachments
  remain supported at the protocol level, not as a default user-facing gate.
- Drawer tabs start expanded on both screen sizes, without pane counts. Right-aligned + icons
  create workspaces on computers, tabs in workspaces, and panes in tabs; pencil
  icons rename the corresponding workspace/tab. New creations become selected.
- Space/tab rename, new space/tab/pane, and right/down terminal splits are shared with
  the desktop through `doc.get/set` **`herdr-app`** separately on each machine.
  Edits preserve unknown JSON fields, validate known structure/references, and
  require the observed document version. Concurrent changes return a conflict
  instead of overwriting somebody else's layout. An ambiguous/failed creation
  may leave an unplaced terminal; the error names it. No automatic kill/retry.
- Selection, focused pane, stack choice and zoom belong only to this browser.
- Core snapshots and ordered events drive terminal facts/status; `missed` replaces
  stale state, and reconnect establishes a fresh snapshot. Machine metadata and
  documents refresh on relevant events and core heartbeats. There is no terminal
  screenshot/text polling, per-read subprocess, legacy relay, or SSH discovery.

Not included: conversation transcripts (no reliable generic agent-session
mapping), gallery window layouts, GPUI mods/native chrome, inline terminal images,
arbitrary terminal file upload, layout drag-resize/reorder, and destructive close/kill
controls. Unsupported mod/floating panes are explicitly marked; no mod code runs.
Pi's native Kitty graphics are not rendered by plain xterm; attaching or
reconnecting also does not replay old images from core snapshots. Do not change
terminal identity to advertise unsupported graphics.
Desktop visual styling is not a pixel-identical port. This is a single trusted
user's web client, not separate accounts or shared-host isolation.

## Development and verification

```sh
make release                    # npm ci + production frontend + release executable
make check                      # typecheck, ESLint, Vitest, fmt, Clippy, Rust tests
make test                       # existing dependency install/assets required
make smoke                      # uv test runner + installed hc; TWO isolated cores
make dev                        # embedded frontend, Rust debug server on port 9001
```

`make smoke` starts disposable cores in a fresh private temporary directory,
federates them with a local `--via` adapter, launches the release executable
**outside the source checkout**, and checks authentication/CSRF, shared local and
remote topology, CAS conflicts, raw terminal command/resize round trips, status
updates, observer reconnect, isolated-core restart and logout. It cleans up only
its own cores. Python/uv are test dependencies, not application dependencies.
Use `uv run --script scripts/smoke.py --keep` to retain failed-test artifacts.

For frontend-only iteration, `cd frontend && npm run dev -- --port 9002` uses a
same-origin API proxy. Start the backend with
`--port 9001 --public-origin http://<browser-visible-host>:9002` and set
`HERDR_API_URL=http://127.0.0.1:9001` for Vite. Never weaken Origin checks to make
Vite work. Prefer the embedded frontend when testing packaging.

Architecture: `src/core.rs` owns persistent RPC and subscription connections;
`src/terminal.rs` translates bounded framed channels to WebSockets;
`src/topology.rs` owns safe document edits; `src/auth.rs` owns browser sessions;
`src/dashboard.rs` owns the pinned terminal launcher. Frontend sources are under
`frontend/src/`. Cargo.lock and frontend/package-lock.json pin build dependencies.

### Touch scrolling

Swipe vertically inside a pane to scroll terminal history with local momentum.
In Pi and other mouse-aware terminal applications, swipes send negotiated wheel
reports instead; lifting your finger stops application input immediately.
Swiping does not open the keyboard. Tap to type. The keyboard accessory strip
also offers **PgUp/PgDn** (open it with the keyboard button on phones).
Applications without mouse support require their own keys; swipes never guess
Up/Down arrow input. Pinch zoom is left to the browser, and reduced-motion
preferences disable momentum.

### Agent commands and favorites

Open the accessory keys and tap **/** beside the paperclip. The command palette
uses the terminal's live core-reported app identity to offer built-in shortcuts
for Pi, Claude, Codex and OpenCode. Pi's loaded extension commands (such as
`/git-mode`), prompt templates and skill commands are also listed when the native
Pi publisher is installed. It is hidden for unrecognized apps and shells;
it does not infer an agent from terminal text or pane names.

Star commands to keep them at the top, or choose **Favorites only**. Favorites
are stored per agent in this browser, not in the shared workspace. Select a
command to edit its arguments, then **Insert** to paste without Enter, or
explicitly **Run** to paste and press Enter. Use an empty terminal prompt for
Run: the existing draft is never cleared. Selection, search and starring do not
send terminal input. A changed connection or reported agent requires reopening
the palette; commands are never queued across reconnects.

The built-in shortcuts are static and version-dependent. Pi's custom command
list comes from `pi.getCommands()` inside the running session, not from scanning
extension source or reading transcripts. Typed command arguments are not saved
as favorites. Favorites for temporarily unavailable commands are retained but
aren't shown or executed until that command is available again.

Install the native Pi publisher on each computer where Pi runs:

```sh
./scripts/install-pi-extension.sh
```

Then use **`/reload` once in existing Pi sessions**, or start a new session. Open
or refresh the Commands palette afterward. The installer adds a separate
`herdr-webapp-commands.ts` symlink under Pi's global extensions directory and
never replaces the older remote client's publisher or another existing file.
`PI_CODING_AGENT_DIR` can select an alternate absolute Pi configuration directory.
Keep this checkout in place while its symlink is installed.

The publisher uses persistent native Core RPC to publish bounded, expiring
command metadata in a dedicated per-terminal versioned document. Discovery does
not type into terminals, execute commands, read session files or use SSH helpers.
Configured remote machines work through native Core document routing; inbound-only
peers cannot provide this catalog. Built-ins remain usable if discovery is
missing, disabled (`HERDR_COMMAND_DISCOVERY=0`), stale or unsupported.
If a `herdr-core/config.toml` file exists, set `HC_SOCKET` explicitly when starting
Pi; the publisher deliberately does not parse TOML configuration. Without that
file, standard Linux/macOS socket discovery works without `HC_SOCKET`.

### Attach an image to a Pi prompt

Open the keyboard accessory panel with the top-right keyboard button, then use
**Attach image** on its second row. Choose a PNG, JPEG, GIF or WebP image up to
8 MiB. The key-sized paperclip sits at the right of the second row and inserts
its path automatically after upload. Add your message and send it in Pi;
insertion never presses Enter or opens the keyboard automatically. This follows Pi's clipboard-image
pattern: stage a file and paste its path; Pi can read it with its file tool.
It is not a direct model-image attachment or a terminal image preview.

Uploads use core's native `terminal.put` on the selected terminal's machine,
including configured remote machines; unsupported peers fail explicitly without
uploading locally. There is no user-supplied destination path or filename.
Files live in core's terminal staging area and are removed when that terminal
record is removed. Privacy relies on core's private state-directory ancestor;
files are never exposed through a public download URL. SVG and HEIC are not
supported; export them to a supported image format first.

Automatic insertion is tied to the trusted file selection and its exact live
connection. If the connection changes during upload, the path is not inserted
or replayed after reconnect; the UI asks you to attach again when connected. Leaving a pane aborts
its browser upload, but an upload already processed by core may remain staged.
Failed/uncertain uploads are not retried automatically. Two uploads can run at
once; staging is capped at 64 MiB per terminal and 256 MiB per web-server run.
These are accident-prevention limits, not a durable disk quota. Core terminal
cleanup or manual removal manages longer-lived staged files.
