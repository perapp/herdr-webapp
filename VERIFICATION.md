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
| `npm test` | 32 tests passed |
| `npm run build` | Passed, Vite 8 / Svelte 5 / xterm.js |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked` | 27 tests passed |
| `cargo build --release --locked` | Passed |
| `npm audit --omit=dev` | 0 vulnerabilities |
| Full `npm audit` | 2 low development-only advisories; no moderate/high/critical |

The two remaining development advisories concern deepmerge and its Svelte Vite
plugin dependency. The registry reported no available upstream fix. Older
critical test-tool dependencies were upgraded, not ignored. Build/test logs:
`minimal-ui-check.log`, `minimal-ui-build.log`, `minimal-ui-smoke.log`,
`minimal-ui-browser.log`, `frontend/audit.log`.

Release artifact: `target/release/herdr-webapp`, **4,015,296 bytes**, Linux x86-64.
SHA-256: `eeb761620d395fa2b7f32ada5b9c494ab48b661c852a405f086d5b3e127e250a`.
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


## Mobile tree actions follow-up

Verified 2026-11-12 against two isolated real cores and an isolated Chromium:

- All tab branches start expanded, with no pane counts or generic Workspace
  actions section in the mobile drawer. Sign-out stays independently accessible.
- Right-aligned + icons create a workspace on a computer, a tab in a workspace,
  and a terminal pane in a tab. Pencil icons preserve contextual rename access.
- Creating from an inactive local or federated tab targets the clicked node, not
  the currently selected pane. A new pane can also be created in an empty tab.
- New panes/tabs/workspaces become privately selected; renames do not navigate.
- Workspace disclosure choices survive incoming state updates; creation controls
  are separate from disclosure summaries and remain usable on collapsed rows.
- Actual keyboard input in a newly created federated mobile terminal, reconnect
  without input replay, and desktop multi-pane restoration/input passed.
- `make check`, `make release`, and `make smoke` passed. The smoke suite now also
  covers real local/federated new-pane creation and stale creation rejection.

Evidence: `tree-actions-check.log`, `tree-actions-build.log`,
`tree-actions-smoke.log`, `tree-actions-browser.log`,
`test-results/tree-actions-drawer.png`, `test-results/tree-actions-desktop.png`.
These browser checks do not establish physical iOS/Android keyboard acceptance.

## Touch scrolling follow-up

- Added frame-batched local scrollback with fractional-row accumulation and
  decaying momentum; reduced-motion disables inertia. Gesture state is cleared
  on disconnect, mode changes, hidden pages, cancellation and disposal.
- Added trusted-touch → xterm wheel encoding for mouse-aware apps, including
  installed Pi's SGR wheel protocol. Application reports have no momentum and
  are classified as intentional input, not automatic terminal replies.
- Added PgUp/PgDn accessory keys. Apps without mouse reporting receive no guessed
  arrow sequences. Swipe clicks are suppressed while taps remain interactive.
- Eight gesture unit tests cover local batching/momentum, bounded wheel reports,
  trust, tap/drag threshold, click suppression, pinch, disconnect/mode changes,
  reduced motion and cleanup. Accessory key payloads are also tested.
- Isolated Chromium at desktop and phone sizes passed trusted terminal typing,
  local touch scrolling without PTY input or keyboard focus, and typing after
  reconnect. An isolated real PTY application negotiated Pi's SGR mouse protocol
  and received touch-generated wheel reports as `input`, never `automatic`.
- `make check`, `make release`, and `make smoke` passed with 40 frontend tests
  and 27 Rust tests. No live user core or terminals were used for verification.
- These checks do not establish physical iOS/Android scrolling feel, nor do they
  constitute an end-to-end Pi conversation test on a phone.

## Persistent sessions and Agents tree follow-up

- Browser cookies remain opaque, HttpOnly, SameSite=Strict session IDs, with the
  existing seven-day expiry. The private `<token-path>.sessions` store contains
  only hashed IDs and a token binding; rotation invalidates previous sessions.
  Atomic durable writes, strict file checks and an exclusive lifetime lock
  protect login/logout persistence. Unsafe or corrupt stores fail closed.
- `make check`, `make release`, and `make smoke` passed: 44 frontend tests and
  34 Rust tests. Smoke now restarts the actual isolated web server after login
  and logout, checking retained-cookie HTTP access, persistent revocation and
  revoked WebSocket rejection; Origin protection is unchanged.
- An isolated Chromium browser retained its actual cookie across a real server
  restart and reload, without a token-bearing URL or another sign-in.
- The burger drawer and desktop rail now show Agents below Spaces, with live
  parent/child hierarchy and reported status. Mapped agents select their exact
  machine/space/tab/pane; unplaced descendants explicitly state that native peek
  is unavailable. Sign-out controls are removed; backend logout remains tested.
- Real isolated local, child and federated agents passed exact-pane selection
  and drawer closing in a phone-sized browser. Hierarchy, unavailable-child
  messaging, order below Spaces, and absence of sign-out controls were checked.
  Desktop/phone input and reconnect also passed. A drawer screenshot was
  inspected in the isolated workstation. No physical-phone acceptance claimed.

## Settings, unified drawer and agent alignment follow-up

- Both desktop and phone use the same burger drawer. Desktop's sidebar and tab
  strip are removed; desktop renders a full selected tab, phone one selected
  pane. Top bar retains the current space/tab and compact secondary actions.
- Settings is in a non-scrolling safe-area-aware drawer footer. Its sheet offers
  System/Light/Dark, persists per browser, follows OS changes only in System,
  and reports unavailable browser storage without breaking appearance changes.
- Terminal themes update in place; native FACTS frames update core viewer colours
  without reconnecting, typing, focusing or claiming resize ownership. Observer
  attachments cannot change facts. Reconnect uses the current preference.
- Agent rows reserve a common disclosure gutter: sibling root names align even
  when one has children. Child indentation and disclosure interaction remain.
- `make check`, `make release`, `make smoke` passed (49 frontend, 34 Rust tests).
  Smoke includes local/federated live theme commands and observer isolation.
- Isolated real Chromium at desktop and phone sizes verified two panes versus
  one pane; no sidebar/tab strip; drawer hierarchy; footer position while a long
  tree scrolls; all appearance modes and OS overrides; localStorage persistence;
  live terminal colours and theme frames without reconnect; terminal input and
  reconnect; root-agent alignment, indentation and expand/collapse interaction.
  Settings and drawer screenshots were inspected. No physical-phone claims.
- Investigated native/web image difference: existing Pi emits Kitty graphics,
  which stock xterm does not render. Native-equivalent image support remains
  explicitly unsupported; no environment/protocol spoofing was introduced.


## Mobile image attachment follow-up

- The top-right keyboard button reveals accessory keys and a second attachment
  row on both desktop and phone. Upload stages the selected image; an explicit
  Insert reference action pastes a quoted path without Enter, using xterm's
  bracketed paste where requested. Upload completion and reconnect never type.
- Native `terminal.put` stages bytes on the selected terminal's actual machine,
  including configured remote cores, without subprocesses, arbitrary destination
  paths, filesystem fallbacks or automatic retries. Unsupported native staging
  fails visibly. Core's state-directory ancestor supplies staging privacy; no
  public image-serving route or image renderer was added.
- The authenticated exact-Origin raw upload route accepts PNG/JPEG/GIF/WebP
  headers, bounds each request at 8 MiB, allows two concurrent uploads and applies
  conservative run-scoped staging budgets. Other API routes retain their 1 MiB
  body limit. Browser checks reject empty, oversized and unsupported picker files.
- `make check` and `make release` passed (55 frontend and 37 Rust tests). The real
  smoke suite passed after adding image upload checks: exact local/federated file
  contents and destination, valid PNG above the standard API body cap, early
  oversized-body rejection, unsupported format, authentication and foreign Origin.
- Isolated Chromium at desktop and phone sizes passed second-row placement,
  trusted file-picker upload, no upload/reconnect input, explicit reference paste
  without newline, and normal input after reconnect. An actual isolated Pi editor
  displayed the uploaded image path in its unsent draft. No inference request or
  user credentials were used. The phone-sized Pi screenshot was inspected.
- Physical iOS/Android picker/camera behaviour remains unverified. HEIC/SVG are
  explicitly unsupported. Inline Kitty graphics and direct model-image attachment
  remain separate unsupported features; this follows Pi's staged-path paste flow.

## Automatic attachment insertion follow-up

- The paperclip is icon-only, right-aligned on the second accessory row, with a
  44×44 target matching the special keys. The confirmation button is removed.
- A trusted file selection authorizes path insertion after upload only on the
  exact original live connection. It never presses Enter or focuses the terminal
  asynchronously. A reconnect/replacement/disposal rejects the stale insertion.
- `make check`, `make release` and `make smoke` passed. Isolated desktop and
  phone-sized Chromium verified button size/alignment, automatic path paste with
  no newline or keyboard focus, delayed-upload rejection after reconnect, and
  successful fresh upload afterward. The phone screenshot was inspected.

## Agent commands and favorites

- Added the `/` key beside the paperclip: static Pi/Claude/Codex/OpenCode command
  catalogs selected only by a live terminal's core-reported `status.app`, never by
  title or terminal-output guessing. Unknown apps and shells have no command key.
- Selection, search and stars send no terminal input. The editable command has
  separate Insert (paste only) and Run (paste + Enter) actions. Neither clears an
  existing draft. Execution requires a trusted click, the original connection,
  and the same reported agent; stale requests are refused rather than replayed.
- Stars are stored per agent in browser localStorage, appear first and can be
  filtered. Tests cover corruption, quotas, blocked storage, obsolete IDs and
  cross-agent preservation. Command arguments are not saved.
- `make check`, `make release`, `make smoke` passed (63 frontend tests). Isolated
  Chromium desktop/390px phone checks verified favorites persistence/filtering,
  no input on selection/star/search Enter, Insert without Enter, explicit Run of
  `/session` in real local and federated Pi editors, the exact remote destination,
  reconnect refusal/fresh-palette recovery, and no palette on a generic shell.
- Inspected phone screenshots at 844px and keyboard-constrained 380px heights;
  compact controls and Run remain visible. Physical iOS/Android keyboards remain
  unverified. Live Pi extension/skill/prompt-template discovery is unsupported.

## Live Pi custom commands

- Added `pi-extension/commands.ts`, installed locally as a separate global Pi
  extension using `scripts/install-pi-extension.sh`. Existing sessions need
  `/reload` once. Installer idempotency and refusal to overwrite existing files
  were verified in private scratch directories. The legacy publisher is intact.
- The extension enumerates `pi.getCommands()` and publishes bounded metadata via
  persistent native Unix RPC into a dedicated per-terminal versioned Core doc.
  No session/source scan, transcript read, subprocess, SSH relay, or new Core
  executable is involved. Updates use CAS, ownership-safe shutdown clearing,
  15-second heartbeats, <=500 commands and <=128 KiB. Socket configuration
  limitations are documented rather than silently guessing a custom config.
- Authenticated `GET /api/commands` accepts only the selected terminal address.
  Native lookup derives machine routing; schema, app, PID/run identity, byte/count
  bounds, 75-second freshness and 5-second future skew are checked. Terminal
  identity is rechecked after reading. Inbound-only peers remain unsupported.
- `make check`, `make release`, `make smoke` passed (69 frontend tests). The smoke
  test now checks exact local/federated command destinations, stale/future/run
  rejection, denied path/on parameters, authentication and exact Origin handling.
- Real isolated Pi sessions loaded the publisher, a harmless fixture `/git-mode`
  handler, a prompt template and a skill. Desktop/phone Chromium discovered all
  three sources, starred the custom command, persisted it across reload, and ran
  the fixture command on the exact local and federated terminal with no model
  inference or real authorization changes.
- Browser tests also checked unavailable-catalog fallback without losing custom
  favorites, no execution from absent favorites, retry recovery, built-ins usable
  during held discovery, and late responses unable to re-enable an old palette
  after reconnect. Phone/keyboard-height screenshots were inspected; physical
  iOS/Android keyboards remain unverified. No live user terminal input was sent
  for verification, and the live Core was not restarted.

## Terminal font-size setting

- Settings now offers 8–28 px terminal text (13 px default), a touch-friendly
  slider, increment/decrement buttons, current value and reset. Preference is
  browser-local, synchronizes panes/tabs, and reports blocked storage honestly.
- Font changes update xterm immediately and recompute the native viewport grid
  even when the pane's pixel bounds are unchanged. No focus, keystrokes or
  reconnect is required; native resize ownership remains authoritative.
- `make check`, `make release` and `make smoke` passed (74 frontend / 39 Rust
  tests). Isolated desktop Chromium verified all panes changing 13→18 px, native
  resize without new sockets or terminal input, and persistence on reload.
  Phone-sized Chromium verified the saved size, slider bounds/disabled controls
  and reset to 13 px. Settings screenshot inspected. Physical phone keyboards
  remain unverified.

## GitHub release installer and updater — pre-publication

- Added authenticated private-GitHub release installation, deterministic per-target
  archives/checksums, a draft-first publishing workflow, and README installer links.
- Added bottom-drawer server version/check/update notice and confirmation popup.
  Updates verify the matching archive and candidate version, preserve configuration,
  and restart only the web server using in-place Unix exec. PID, TTY/process-group,
  stdio and resolved bind/port/Core/token/Origin settings are preserved; listener
  and authentication-lock descriptors close on exec. Backups are retired only
  after a new HTTP bind and validation of the older managed payload.
- Local `make check`, `make release`, `make smoke` passed: 110 frontend tests,
  51 Rust tests and 13 offline distribution tests. Smoke now checks update API/SSE
  authentication, exact Origin and refusal to replace an unmanaged source binary.
- The first release is 0.0.1 consistently across Cargo, lockfiles and frontend
  metadata. Published-asset installation and live baseline-to-release upgrade
  verification are recorded separately after publication.

## Published 0.0.1 acceptance

- GitHub release v0.0.1 contains all three target archives, install.sh and
  SHA256SUMS. Native macOS ARM CI initially found aliased temporary fixture paths;
  a CI-only canonical-temp fix allowed an unchanged v0.0.1 tag to pass the complete
  workflow. The repository remains private; downloads use authenticated gh.
- Downloaded the actual published installer and Linux x86_64 asset, installed into
  an isolated managed location, ran --version, and successfully reinstalled.
  The published installer refused replacement of a live managed web server.
- Updated a separately compiled isolated managed 0.0.0 baseline to the actual
  published 0.0.1 through the phone confirmation. Desktop check/notice/Cancel,
  visible phone confirmation, authenticated reconnect, one-time frontend reload,
  retained output, and trusted post-reconnect command execution passed.
- Verified the same server PID and foreground TTY process group, unchanged shell
  PID/creation identity, matching managed receipt, and cleaned update lock/backup.
- Physical mobile keyboards and Linux ARM installation were not tested. macOS ARM
  ran native CI checks/version probing; interactive installation was not tested.

## 0.0.2 phone-rotation correction

- Portaled the native update confirmation to document.body. The drawer closes
  when crossing the phone/desktop breakpoint; its confirmation must not inherit
  the closed drawer's display:none and leave an invisible blocking modal.
- make check/release/smoke passed again (110 frontend, 51 Rust, 13 distribution).
  Actual published 0.0.1-to-0.0.2 acceptance is recorded after publication.
- An isolated managed preview using the new embedded frontend passed both
  desktop-to-portrait and portrait-to-landscape confirmation rotation: the dialog
  stayed visible outside the closed drawer, Cancel removed it, and navigation
  remained usable. The existing 0.0.1 browser session was preserved afterward.

## Published 0.0.2 acceptance and deployment

- All three v0.0.2 target archives, installer and SHA256SUMS published successfully
  from the tagged source. Linux ARM remains a cross-build; macOS ARM ran native
  host checks and the executable version probe.
- The phone-sized isolated browser performed a genuine published 0.0.1 → 0.0.2
  update, retaining authentication without re-entering its token, automatically
  loading the new frontend once, preserving terminal output, and executing a
  distinct trusted post-reconnect command. Automatic checking caused no reload
  loop. Desktop final-version controls and portrait/landscape screenshots were
  inspected; four screenshots remain in ignored dist/verification/v0.0.2/.
- Across both upgrades, server PID and foreground TTY group stayed unchanged;
  the isolated shell retained its PID and creation timestamp. Receipts advanced,
  validated backups and update locks were cleaned, and further updates remain
  enabled. Both Cancel and installer refusal of live replacement passed.
- Installed the actual published 0.0.2 asset to the default per-user managed
  location (~/.local/share/herdr-webapp, CLI ~/.local/bin/herdr-webapp). Switched
  the existing LAN server pane to that managed executable on 0.0.0.0:8001.
  The old source server required SIGTERM after its unbounded graceful drain;
  the new release has the tested bounded drain. Only the web process restarted.
- Live access-token bytes and durable browser-session store were unchanged;
  native Core remained connected and the existing Pi terminal was preserved.
  The live server reports managed 0.0.2, successfully checked private GitHub,
  and is already on the latest release. Existing pages need one refresh to load
  the update menu; future in-app updates reload it automatically.
- All isolated cores/terminals, preview servers, GUI workstation and temporary
  build/download/auth fixtures were stopped and removed. Only the live managed
  server was retained. No physical iOS/Android keyboard verification was done.
