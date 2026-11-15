# Isolated relay browser acceptance

Run from a Herdr **tasks** pane, not the live Core's terminal fixture. This runner
starts its own Core, shell, document, relay, connector, GUI workstation and (for
TURN cases) rootless coturn. It never uses the user's Core socket or terminal.

```sh
python3 scripts/relay-e2e.py \
  --gui-cli /absolute/path/to/gui-workstation \
  --binary target/release/herdr-webapp --transport direct --port 9001
```

For a focused pane-switch loop, add `--focus-pane-switch`: it checks desktop
input, Connections/Settings/Versions source links, phone pane switches without
peer replacement or start-page flashes, and authenticated reconnect. It also
checks exact LF from Ctrl J, the lightning command opener, computer details,
the native agent sort menu, unread-agent jump/count/acknowledgement and previous-pane
toggling (without focusing terminal input), plus parsed CSS and actual glyph pixels
for release-inlined program icon masks. Desktop focus switches and responsive
layout changes are checked too: ordinary switching must retain the App/workspace DOM
and never show reconnect or empty/onboarding screens; explicit reconnect must
never flash a loaded-empty workspace before metadata arrives, and the previous-pane
button must still toggle correctly after the authenticated workspace remounts. This is not
a substitute for the full upload, editor and access-rotation suite.

Repeat with `--transport udp`, `--transport tcp`, and `--transport tls`. Each
invocation owns a fresh fixture. Default binary: `target/release/herdr-webapp`;
default transport: direct. TCP publishes **only TCP TURN**; TLS publishes **only
TLS TURN over TCP**, with plaintext TCP/UDP, DTLS and CLI listeners disabled.
Neither substitutes a UDP TURN URL if native stream transport fails. The finite
UDP relay-allocation port range is separate from the TURN control listener.

## Existing public relay / LAN-hairpin acceptance

Use `--relay-origin https://your-relay.example` with an approved executable via
`--binary /private/catalog/0.0.5/x86_64-unknown-linux-gnu/herdr-webapp`. This mode
still creates its own Core, shell, connector, pairing and GUI; it does **not**
start/reconfigure a relay or coturn, and it never bypasses browser certificates.
Coordinate any deployed ICE configuration change with its operator beforehand.

For `--transport udp` or `tcp`, both endpoints are forced to relay. The browser
asserts that the relay advertises only the requested TURN transport, as well as
inspecting both selected candidate types and its selected relay protocol. This
avoids calling browser-TCP/native-UDP success a native TCP test. Run separate
UDP-only and TCP-only phases, then have the operator restore normal configuration.
An external hostname from the same LAN can exercise router hairpin forwarding,
but does not establish cellular, external-network or physical-phone acceptance.

## Prerequisites

- Linux/Pasta networking; literal host loopback must be reachable from the GUI.
  The runner pins the GUI network to Pasta with a distinct `10.0.2.100/24`
  guest address (so host LAN ICE candidates do not loop back into the guest),
  disables automatic image upgrades,
  and acquires a new workstation with an explicit harness owner PID.
- Installed `hc`, release executable, and explicitly provided GUI CLI.
- Existing GUI image. The GUI CLI controls its normal acquire lifecycle; provision
  it beforehand to avoid an initial image build during acceptance.
- For TLS: installed `openssl` for private short-lived certificates.
- For TURN: rootless Podman and locally available `docker.io/coturn/coturn`.
  The runner checks image availability and uses `--pull=never` throughout.
- Host Python uses only the standard library; the GUI image supplies
  `websocket-client`, PIL and Chromium.

## Assertions

- Pair capability disappears from the URL; actual selected ICE pair is inspected.
  Direct requires neither candidate to be relay; TURN requires **both** relay and
  the browser's selected relay protocol to match the requested transport. Native
  `--force-turn` and browser `forceTurn=1` are both set for TURN cases.
- Trusted CDP terminal input produces a real, independently checked native file.
- A real picker upload plus 20 sequential uploads stay on the same peer epoch;
  each native file is distinct, inside the disposable Core tree, and byte-equal
  to the original PNG. A shell draft before **each** upload detects accidental
  Enter; trusted Ctrl+U discards drafts. A subsequent shell sentinel drains input
  before checking the forbidden file.
- Reload uses persisted pairing, not a fresh capability; closing the peer tests
  automatic authenticated resume. Both produce independently checked native files.
  Each action opens a new CDP session. Reload registers the passive observer on
  that session **before** navigation, rather than relying on the original pairing
  session's new-document registration or injecting after the app creates its peer.
- A printed HTTP(S) link opens a real client-browser tab directly, with no in-app dialog.
- Desktop and phone-sized raw-PTY editors retain a partial draft across tool
  completion repaint, CPR/DA/color queries and focus modes; only explicit Enter
  submits it. This does not prove every physical phone IME or external agent
  message-injection path.
- Connections root remains a list with one remembered computer; Settings remains
  available before pairing, with Install app inside it. The Herdr-webapp menu
  heading links back to Connections. `/launch` resumes the
  explicit computer and accepts trusted terminal input after authenticated reload.
- Phone-sized input and reconnect are checked with Chromium device metrics/touch
  emulation, not claimed as physical Android/iOS keyboard verification.
- Two real fixture terminals and repeated pane switches must retain the
  same peer epoch and connection route. A DOM observer checks for transient
  Connections/sign-in flashes throughout switching and an actual peer reconnect;
  a shell-file proof confirms input still works after switching.
- Two independent connector identities share the disposable Core/layout IDs on
  purpose. Both remain remembered; explicit route switching authenticates each
  pinned identity without re-pairing and produces independent shell file proofs.
- Two independent incognito profiles reuse the original private capability. A
  confirmed Settings rotation keeps the initiating phone-sized profile on the
  same live peer, closes the other profile, rejects its resumed key and the old
  capability, and leaves the second computer’s existing key and original link
  usable. Desktop/390px Settings screenshots and shell proofs stay private.
- 400,000 output characters traverse the terminal beyond its initial output
  credit, followed by an unambiguous sentinel and native file proof.
- Waits require attached/live terminal state, not merely textarea DOM existence.
  Frame observation is passive; no application endpoints or transports are mocked.

TURN uses a private owner-only config and REST secret file. A no-network,
`--user 0` preflight verifies the 0600 mount is readable **before any port is
published**, avoiding the coturn image's default unprivileged UID. The live coturn container uses rootless host networking (not the GUI container),
a loopback-only listener and a finite LAN UDP allocation range, avoiding a second
Pasta NAT around relay sockets. Its file capability requires retaining only
NET_BIND_SERVICE in the capability bounding set; all other capabilities are
dropped. Listener/allocation ports and quotas are finite. A real STUN challenge checks realm and
nonce, followed by a REST-authenticated allocation, before the relay starts.
Advertised IPv4 comes from local routing lookup (no outbound probe packet).

TLS creates an owner-only local CA and leaf certificate with `IP:127.0.0.1` SAN.
CA signing material is deleted before mounting; only the dedicated 0700 TURN
subdirectory (config, CA certificate, leaf certificate/key) enters coturn, never
the isolated Core tree. The native fixture processes alone receive
`SSL_CERT_FILE=<private CA>`: native rustls root and hostname/IP verification
remain mandatory, with no insecure native option. The real TLS 401/realm/nonce
and authenticated Allocate probe uses Python `ssl.create_default_context(cafile=...)`
and `server_hostname='127.0.0.1'`, with stream framing and no UDP fallback.

**Isolated-browser exception:** TLS launches only its newly acquired disposable
Chromium with `--ignore-certificate-errors`, to accommodate the private CA without
changing host/global browser trust. This is NOT a native TLS bypass. ICE assertions
still require both selected candidates to be relay and browser `relayProtocol=tls`;
native `--force-turn` remains mandatory.

## Output and cleanup

Only a concise allowlisted candidate/protocol summary and assertion names are
printed. Child stdout/stderr, page text, exceptions, URLs, SDP and acquired GUI
JSON are never printed. `--artifacts /path` exports a sanitized JSON report only
(success or failed phase); failures also print a static allowlisted assertion
label, never arbitrary browser exception messages. It exports no raw screenshots or private
workspace files. Screenshots can contain terminal paths and pairing screens and
are therefore not retained automatically.

SIGINT, SIGTERM and exceptions trigger cleanup of owned process groups, isolated
Core, uniquely named coturn, acquired GUI workstation, and private temporary
files. Cleanup failures are named without raw diagnostics. SIGKILL/host crashes
cannot run Python cleanup; use the GUI CLI's owner/idle lifecycle for recovery.
No broad container pruning, live-Core restart, or global software installation.

## Offline validation

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/relay-e2e/test_static.py
python3 scripts/relay-e2e.py --help
```

Offline tests cover syntax, private file behavior, environment isolation/cleanup,
in-memory TURN challenge/HMAC framing (UDP and partial TCP/TLS reads), verified
TLS context/server-name setup, fail-closed certificate errors, reload observer
registration ordering, and real local OpenSSL certificate SAN validation. They
start no real Core, relay, browser, listener, or container. Live acceptance must
be run separately; offline success is not evidence of working TCP/TLS TURN or
physical phone keyboards.
