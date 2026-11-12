# Herdr Web

The native-phone-app experience of **Herdr**, delivered through a browser.
Packaged as one Rust executable with the
Svelte/TypeScript/xterm.js frontend embedded. Core remains the terminal authority;
the browser shares the desktop's machine-owned spaces, tabs and panes. The desktop
application does not need to be running.

## Start

Build prerequisites: **Rust 1.90+**, **Node 24+**, npm, and Make. Runtime: a running
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
- Successful sign-in creates a random, in-memory, HttpOnly, SameSite=Strict
  session cookie, valid for seven days. Server restart invalidates sessions.
  Sign out revokes that session, including open streams (within five seconds for
  terminals and fifteen seconds for state streams). Access tokens are not logged
  by API handlers. **Both interactive and plain launcher output contain the private URL and QR.**
- Keep the QR, token file and captured launcher output private. To revoke the
  access token, stop this web server, remove its token file, and restart it.
- All browser mutations and WebSocket upgrades require the exact same Origin.
  There is no permissive CORS, and neither `Forwarded` nor `X-Forwarded-Host` is
  trusted. The UI uses escaped text, a restrictive CSP, and no mod execution.
- **Default HTTP is unencrypted.** Use a trusted LAN/VPN, an SSH tunnel, or an
  HTTPS reverse proxy. Do not expose this directly to the Internet. For a proxy,
  set `--public-origin` to the exact external origin (no trailing slash); this
  also enables Secure cookies. Proxy WebSockets and SSE without buffering.

## Initial app

- Terminal-first native-style chrome: a 40 px desktop tab strip, narrow collapsible
  spaces sidebar and compact pane titles; no large headers or persistent action
  toolbars. Secondary commands live under the ellipsis menu.
- Mobile uses one 44 px top bar and one pane at a time. The burger drawer navigates
  computers → workspaces → tabs → panes, including inactive stack panes. Selecting
  a pane does not alter shared layouts. Private selection survives reload.
- Light/dark appearance follows the system, including terminal colors and the
  display facts advertised to core. Desktop retains recursive multi-pane layouts.
- Real xterm terminals: raw framed attach output, keyboard/paste, focus and PTY
  resize. Byte credit is replenished only after xterm parses the output.
- A native-phone-app interaction model: terminals are **interactive immediately**.
  Tap the terminal to type; no Take/Release control workflow. On phones, the
  keyboard icon reveals accessory keys for Esc, Tab, Ctrl C and arrows. Reconnect automatically restores input
  without replaying disconnected keystrokes or opening the keyboard unprompted.
  The core arbitrates resize ownership; browser terminals follow its authoritative
  dimensions rather than fighting other clients. Explicit observer attachments
  remain supported at the protocol level, not as a default user-facing gate.
- Space/tab rename, new space/tab, and right/down terminal splits are shared with
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
terminal file/image upload, layout drag-resize/reorder, and destructive close/kill
controls. Unsupported mod/floating panes are explicitly marked; no mod code runs.
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
