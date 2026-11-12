# Implementation verification

Verified on **perl**, initially 2026-11-11 UTC and updated 2026-11-12 UTC, in `/home/per/Projects/herdr-webapp`.
No reference repositories were edited, no system service was installed, and the
live core was not restarted.

## Build and automated checks

| Check | Result |
|---|---|
| `make check` | Passed |
| `npm ci` | Passed using `frontend/package-lock.json` |
| `npm run check` | 0 errors, 0 warnings |
| `npm run lint` | Passed, warnings denied |
| `npm test` | 30 tests passed |
| `npm run build` | Passed, Vite 8 / Svelte 5 / xterm.js |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked` | 25 tests passed |
| `cargo build --release --locked` | Passed |
| `npm audit --omit=dev` | 0 vulnerabilities |
| Full `npm audit` | 2 low development-only advisories; no moderate/high/critical |

The two remaining development advisories concern deepmerge and its Svelte Vite
plugin dependency. The registry reported no available upstream fix. Older
critical test-tool dependencies were upgraded, not ignored. Build/test logs:
`minimal-ui-check.log`, `minimal-ui-build.log`, `minimal-ui-smoke.log`,
`minimal-ui-browser.log`, `frontend/audit.log`.

Release artifact: `target/release/herdr-webapp`, **4,009,248 bytes**, Linux x86-64.
SHA-256: `6d46b59f0318842782f8105425237e1442fde10db3d9a8a684fbd925bfee2596`.
The executable embeds its assets; Linux system libc/libm/libgcc remain dynamic
platform dependencies. It was launched outside the checkout during acceptance.

## Real isolated-core acceptance

`uv run --script scripts/smoke.py --binary "$PWD/target/release/herdr-webapp"`
passed against installed **herdr-core 0.1.19**. It starts two private local core
sessions and federates them through native core `--via` transport (no SSH relay).

Verified:

- Embedded frontend served from a binary launched in a fresh scratch directory.
- Unauthorized API/WS rejection, valid/invalid login, HttpOnly/SameSite cookie,
  foreign-Origin mutation and WebSocket rejection, logout and open-stream revocation.
- Local and federated terminal discovery and shared document routing.
- New spaces, tab rename, split, unknown JSON preservation and stale CAS rejection.
- Actual PTY keyboard/output round trips and `stty size` on both cores.
- Observer first paint/reconnect, ignored observer keyboard/focus/resize messages.
- 600,000 bytes of terminal output, exceeding the initial stream credit window.
- Federated OSC 7501 status changes via events, not screen/text polling.
- Explicit disconnect and fresh-snapshot reconnect after restarting only test core A.
- Cleanup removes federation before stopping peers, preventing auto-restart races.
  Final verification left no `herdr-web-smoke-*` scratch directories.

## Isolated Chromium

A dedicated rootless GUI Workstation was acquired and released. No pointer or
keyboard automation touched the real desktop. Verified the release frontend at
1440×779 and a 390×844 mobile viewport:

- Cookie and fragment-link sign-in; token fragment removed, cookie not readable
  by JavaScript, no token in browser storage.
- Local and federated space/tab names, reported status dots and live terminal panes.
- Genuine key events through xterm executed local and federated shell commands;
  output markers were observed in the browser.
- Shared new-tab dialog created a real remote terminal.
- Initial acceptance verified private selection persistence and observer recovery.
  Interactive behavior is superseded by the native-phone follow-up below.
- Mobile layout stayed within 390 px document width.

Synthetic-fixture screenshots: `test-results/desktop-terminal.png` and
`test-results/mobile-terminal.png` (ignored build/test artifacts).

## Native-phone interaction follow-up

`AGENTS.md` records the product vision: a native-phone-app version of herdr-app,
not an observer dashboard or an approval-based desktop-control workflow. Native
source inspection confirmed ordinary interactive attachments and automatic
interactive reattachment, with core activity-based sizing rather than an
exclusive keyboard lock. The browser now follows that model:

- Interactive attach by default, including API requests that omit `observe`.
- No Take control / Release control buttons or read-only reconnect step.
- Tap the terminal/header to focus; no forced keyboard focus during attach.
- Phone accessory keys for Esc, Tab, Ctrl C and arrows, with 44 px touch targets.
- Follow authoritative core resize notifications; only actual pane geometry
  changes propose sizes, avoiding scrollbar/remote-paint resize feedback loops.
- Trusted input/paste payload matching survives DOM listener microtask boundaries
  and asynchronous IME completion, while parser replies remain automatic.
- Disconnected input is disabled and never queued or replayed on reconnect.

Fresh isolated Chromium checks passed at 1440×779, 390×844 and 390×600. Actual
browser keyboard events executed local and federated commands without a mode
switch; trusted Unicode IME input used the intentional channel; the Ctrl C key
interrupted a running sleep; forcibly dropped WebSockets automatically recovered
interactively without replaying input. There were no mode buttons or forced
initial textarea focus. The page stayed within phone width. Physical iOS/Android
keyboards were not available and are not claimed as tested.

Evidence: `native-phone-browser.log`, `test-results/native-desktop.png`,
`test-results/native-mobile.png` and the synthetic browser test script in
`test-results/native-phone-browser.py`. Test fixtures and GUI workstation were
cleaned up; the user's core was not restarted.

## Minimal native-style layout follow-up

The user's native herdr-app screenshot guided light/dark chrome and proportions:
40 px desktop bar, 216 px collapsible spaces sidebar, compact 28 px pane titles,
and no persistent workspace headers, action toolbars or footer. Desktop retains
its recursive multi-pane layout. Actions remain available in an ellipsis menu.

Mobile conditionally mounts only one selected pane, not hidden duplicate desktop
terminals. It uses a 44 px top bar with burger navigation and optional keyboard
keys. The native modal drawer lists computers → workspaces → tabs → panes,
including inactive stack leaves. Navigation stays private, reveals ancestor stack
selections and safely resolves stale/hidden pane IDs. Unsupported leaves remain
explicit. The viewport tracks visualViewport height for phone keyboards.

Fresh isolated browser acceptance passed:

- At 390×844: one terminal, no pane header or accessory row by default; terminal
  top 44 px and height 800 px. No horizontal page overflow.
- Drawer navigation switches local panes and reaches the federated computer;
  selection survives reload and exactly one active terminal stream remains.
- Optional Esc/Tab/Ctrl C/arrow keys toggle on/off; modal Escape dismissal works.
- A 390×500 keyboard-sized viewport and 844×390 phone landscape retain one pane.
- WebSocket loss recovers interactively; real local/remote command round trips.
- Returning to desktop restores multiple panes and the native-style tab/sidebar
  layout. Both light and dark chrome/terminal colors were visually inspected.
- New-tab and split dialogs still open from the compact actions menu; desktop
  sidebar collapse works. The user's core and terminals were not used as fixtures.

Evidence: `minimal-ui-browser.log`, `test-results/minimal-desktop.png`,
`test-results/minimal-mobile.png`, `test-results/minimal-drawer.png`, dark variants,
and `test-results/minimal-ui-browser.py`. Physical iOS/Android devices were not
available; phone keyboard behavior beyond the emulated viewport is not claimed.

## Login diagnostics follow-up

The initial frontend hid all sign-in failures behind one generic message. API
errors now use structured JSON, and the browser preserves both JSON and older
plain-text error reasons. Pasted token whitespace is trimmed. LAN IP/hostname
login was verified; regressions cover LAN/mobile-origin acceptance while foreign
origins remain denied. The live endpoint returns 200 for valid LAN sign-in, 401
with `Invalid access token` for a wrong token, and 403 with an explicit origin
reason for a foreign site. The user's specific phone failure has not yet been
identified; the updated UI exposes the reason instead of guessing. No CORS or
origin protection was disabled. Only the web server was restarted; token/QR and
LAN URL were retained.

## Token-bearing launcher URL

Both the pinned LAN URL and QR now use the same private `/#token=…` sign-in
link. Plain output uses that same link, and smoke verification asserts its token
fragment matches the configured access token before testing authentication.
Pinned URLs wrap without dropping token characters, with OSC 8 hyperlinks that
preserve the complete target across wrapped rows. Launcher warnings explicitly
cover both URL and QR. Tokens remain absent from HTTP request URLs and API logs.

## Running application

The final release is running at **http://192.168.1.234:8001**, bound to
`0.0.0.0:8001`, in terminal **t3g** in this space's **tasks** tab. Temporary verification panes
were removed; unrelated panes were left untouched. The LAN URL and private authentication QR stay above
the bounded logs. Credentials are deliberately absent from this report.

Authenticated read-only discovery against the real local core showed all three
current desktop spaces, including `herdr-webapp`; unauthenticated requests
returned 401. The live core PID remained **2131927**, version **0.1.19**, and the
implementation terminal remained **188×44**. Original desktop focus was preserved.
The real local core has no configured outbound machines; federation was verified
with the two real isolated cores, not a separately configured external host.

Remaining scope: no generic conversation transcript identity mapping, mods,
gallery/native window layouts, terminal image/file upload, drag layout editing,
or destructive close/kill UI. Browser terminal rendering is xterm, not Ghostty's
native renderer. HTTP is unencrypted unless placed behind an HTTPS proxy/VPN.
The Mac coordinator's acceptance/completion reports were attempted but denied by
core's cross-machine callback policy; no routing policy was bypassed.
