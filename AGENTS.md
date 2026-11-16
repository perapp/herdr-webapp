# Herdr web app

## Product vision

The web app should feel like the **native phone-app version of herdr-app**:
a first-class Herdr client for using your terminals from a phone, not a remote
viewer, administration dashboard, or desktop-control approval workflow.

- Keep UX as close to herdr-app as possible: interaction model, terminology,
  status meanings, colors and indicators. Adapt deliberately for touch and a
  small screen; this is its phone-app version, not a separate UX. Spaces, tabs,
  panes and live terminals are the app.
- After authentication, a terminal is interactive immediately. Tapping it focuses
  input and opens the device keyboard. Do not add Take control / Release control
  steps or default read-only modes unless the user explicitly requests them.
- Reconnect restores normal interactivity automatically; never replay or queue
  keystrokes while disconnected, or open the phone keyboard without a user gesture.
- Use the core's native input/resize ownership semantics. Do not invent exclusive
  control locks, permissions or ownership prompts. Avoid resize/focus fights
  between clients and panes; distinguish parser-generated terminal replies from
  intentional input.
- Maximize terminal area on both phone and desktop. On phones, show only a compact
  top bar and one pane; put computer → workspace → tab → pane navigation in the
  burger drawer. Keyboard accessory keys are available on demand. Desktop should
  closely mimic herdr-app: a narrow spaces sidebar, slim tab strip and compact
  pane titles, with secondary actions tucked away instead of persistent toolbars.
- Prioritize useful terminal space, touch-friendly actions and access to keys
  missing from phone keyboards. Keep technical status unobtrusive; errors should
  explain what happened and how to recover.
- Browser navigation/focus/zoom remain private. Structural edits share the
  versioned per-machine herdr-app document and preserve unknown fields.

## Fast development

- UI loop: `make dev` (Vite HMR + disposable Core; never the user's Core).
- `make check-fast`; focused tests: `make ui-test TEST=fontSize.test.ts` or `make ui-watch TEST=fontSize.test.ts`.
- Reuse the debug backend for UI edits; `make dev-build` + restart dev after Rust changes. `make setup` forces dependency refresh.
- Reuse `scripts/relay-e2e/browser.py` helpers; wait for actual reload/disconnect state, not sleeps or positional selectors. Same-profile tabs share saved navigation.
- `make ui-e2e` drives a headless phone and desktop Chrome against a disposable Core (keyboard, scrolling, selection, attach, Settings); extend `scripts/ui-e2e.py` when a browser-only behaviour changes. It also runs as part of `make smoke`.
- Run full `make check && make release && make smoke` once at the final gate. Local HMR does not prove relay/TURN behavior.

## Engineering and verification

- Work in this project, not the inherited terminal's misleading launch directory.
- Deliver one executable with embedded frontend assets and a simple ./run.sh.
- Use persistent direct core RPC, subscriptions and framed terminal streams.
  No screenshot/text polling, per-read subprocesses or legacy adapters.
- Authentication grants full trusted-user shell access. Keep token/session safety,
  exact Origin protections and escaped rendering; usability is not a reason to
  weaken them. Both the pinned LAN URL and QR must embed the same access token.
  Never print the real token, private link or QR in reports or test logs.
- Never restart the user's live core or kill user terminals for verification.
  Use isolated cores and an isolated GUI workstation, not real-desktop automation.
- Run long jobs and the persistent LAN server in this space's tasks tab. Preserve
  desktop focus. Keep the private URL/QR pinned above server logs.
- Run make check, make release and make smoke after relevant changes. Verify
  terminal input and reconnect in desktop and phone-sized isolated browsers;
  synthetic tests do not prove behavior on physical iOS/Android keyboards.
- Reference implementations: ~/Projects/ext/herdr-app and
  ~/Projects/ext/herdr-core. Explicitly mark unsupported functionality rather than
  faking native compatibility. No commits or pushes unless requested.
