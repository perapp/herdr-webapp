# Installed apps and remembered computers

Herdr has one installable app per release channel: **Herdr** (stable) and **Herdr Main**. Their manifest identities/start URLs are `/launch` and `/main/launch`. Ordinary `/` and `/main/` visits always show Connections. An icon's cold launch restores the channel's last explicit computer selection, without an application-imposed expiry. Direct connection links, pairing links and notification destinations take precedence. Existing windows keep their view and reconnect normally.

Computer routes contain a public machine identifier, never authorization. Browser IndexedDB keeps the existing pinned connector identity and nonextractable signing key; authentication and exact-Origin checks remain in place. Selection and private space/tab/pane state are channel/computer scoped. One computer is active per window; separate windows can connect concurrently. An offline computer remains selected, with Retry and Connections actions. A missing record explains how to pair again. Local forgetting clears this browser's view, selection and push worker; it does not revoke other devices or connector-side authorization.

Browser data eviction/clearing, private browsing, separate installed-app storage and revoked authorization can require fresh pairing. Remembering a destination does not extend credentials. No terminal input is queued/replayed on reconnect, and launch/notification navigation never intentionally focuses the keyboard.

## Installation

**Install app** is always available inside Settings on both desktop and phone, including before pairing and while disconnected. Install app and Version stay visible in the Settings footer while its Display and Notifications sections scroll. The **Herdr-webapp** name at the top of the navigation menu returns to the current channel’s Connections list; there is no separate Connections menu action. Chromium's retained native prompt is invoked directly from the click when offered. Otherwise an accessible dialog explains the detected platform's manual steps, standalone state, secure-origin limitation, embedded-browser limitation or unknown reason. A normal tab cannot reliably detect every installation elsewhere.

- Android Chrome: native prompt when offered; otherwise browser menu → Install app / Add to Home screen. Samsung Internet wording/capabilities vary.
- iPhone/iPad: browser Share → Add to Home Screen; Safari is a practical fallback when that entry is missing. Desktop-style iPad identification is handled.
- macOS Safari: File → Add to Dock where supported.
- Restricted/unknown browsers: an explanation is shown, never a silently hidden action.

The service worker handles push/click only: **no fetch interception or offline cache**, no authenticated response caching, no forced activation, no client claiming and no automatic terminal reload. A disconnected app needs connectivity to load; it is not an offline terminal. Stable/main prefixes organize behavior, not a same-origin security boundary.

## Notifications

See [web-push.md](web-push.md) for server architecture, authenticated API, key storage, endpoint restrictions and deployment requirements. Settings opt-in is per paired computer/browser, permission is requested only from that click, and disable removes the intended authenticated subscription and unsubscribes its browser endpoint. Per-computer push-only worker registrations allow independent connector VAPID keys and notifications from enabled but unselected computers. The app-shell registration is still per release channel, not a separate installation per computer.

Generic lock-screen alerts contain no terminal text or identifying labels. A short per-page foreground lease suppresses only the exact actively viewed terminal while visible/focused. Clicking routes to the public computer/terminal identifiers after normal authentication, reveals the corresponding pane without opening input, or explains a missing pane. The destination overrides remembered launch selection.

Android Web Push does **not** generally require Home Screen installation. On iPhone/iPad it requires an installed Home Screen app and iOS/iPadOS 16.4+. Delivery is best-effort: connectivity, permissions, OS Focus settings, battery/vendor restrictions and force-stopping browsers can prevent it. Browser permission, app preference and subscription existence are shown separately; existence is not a delivery guarantee. This UI configures notifications through paired relay computer routes; raw local-mode endpoints exist, but a direct LAN page is not a replacement for a trusted HTTPS relay installation.

## Verification boundaries

Automated coverage includes channel routing, indefinitely remembered selection, isolated views/late events, manifest identity, install prompt lifecycle/manual instructions, worker validation/click routing, notification registration/permission/removal, transport capture, target-pane selection, and backend observer/auth/storage/encryption behavior. Full test/build/smoke and isolated-browser results are recorded in the implementation handoff/verification notes.

| Platform | Physical verification |
| --- | --- |
| Chrome for Android | Not available in this environment; install, closed-app push, click routing and keyboard need device testing; no version claimed |
| Samsung Internet | Not available; supported endpoint/provider combinations and OS behavior remain unverified |
| iPhone/iPad Home Screen app | Not available; install, background push and physical keyboard behavior remain unverified; no version claimed |

Browser emulation and isolated desktop Chromium do not establish physical mobile behavior. No real browser push-service subscription or physical background delivery is claimed by unit tests alone.

### Recorded local results

- `make check && make release && make smoke`: passed on the final implementation. Frontend: 237 tests; backend: 140 passed, 2 intentionally ignored; TURN stream adapter: 7 focused tests. Distribution/launcher/channel/dev static checks also passed.
- `python3 -B scripts/relay-e2e.py --gui-cli <gui-workstation> --binary target/release/herdr-webapp --transport udp --port 9001`: passed with both selected candidates confirmed relay/UDP. Real isolated PTY file proofs covered input, reload, authenticated reconnect, Connections root, icon-launch selection restoration, phone-sized input/reconnect, two remembered connector identities sharing layout IDs, 21 uploads without accidental Enter, and output beyond 400 KB.
- Direct ICE in this host's isolated browser fixture did **not** establish a terminal. The same failure was reproduced with an unchanged HEAD build and its unchanged original single-connector runner. This establishes a baseline fixture limitation, not its underlying network cause; direct ICE acceptance remains open here.
- Public stable HTTPS HEAD returned HTTP 200 with HSTS during implementation. No production deployment, existing LAN-server restart, or live user-Core restart was performed.
- Independent review fixes were covered by tests: bounded asynchronous push delivery and lag recovery, removal-churn baseline pruning, dotted native machine addresses, and waiting for fresh layout metadata before consuming notification targets.
