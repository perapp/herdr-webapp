# Relay implementation verification — source 0.0.8

This records implementation/local acceptance, **not public deployment approval**.
Tests used disposable Cores and owned GUI workstations; the live user Core and
managed 0.0.2 LAN server were left untouched. A separate edge agent owns actual
hosting/DNS/router deployment. Release publication is separate from these results.

## 0.0.8 terminal QR follow-up

The immutable v0.0.6 tag (`586f933`) was not published: macOS CI caught a
const/mutable null-pointer mismatch in the disposable `openpty` test. Version
0.0.7 uses mutable null pointers compatible with both macOS and Linux; this
changes the fixture and version metadata, not the PNG runtime design. Its
immutable tag (`bb780af`) also remains unpublished: Darwin sets the transient
kernel-owned `PENDIN` flag when canonical input is restored. Version 0.0.8 allows
only that flag to differ on macOS, retaining strict configured-mode/echo/signal,
control-character, speed and shared-file-flag restoration checks.

These dashboard changes are **published and deployed as v0.0.8**. They were not
part of the earlier v0.0.5 executable/catalog. Verification and provenance:

- Immutable tag `v0.0.8`: `81f8a9ceabfe2bb5509aa84039f33432d70bc5f4`.
  Release CI **43589940697** passed all three platform builds and publication.
- Approved three-platform catalog: ignored `dist/relay-catalog/0.0.8/`;
  prepared manifest SHA-256
  `b3b62238d0dc0fcceb60d04cd9b2a5c163df1c8e98ed9907856e99bbf9a6b85f`.
  All private catalog permissions, archive checksums and native exact-version
  probe passed. Edge staged the catalog's exact CI x86_64 binary, retaining
  v0.0.5 rollback artifacts; no TURN/router changes were made.
- Independent trusted-HTTPS verification after activation confirmed API version
  0.0.8, bootstrap endpoints, exact approved manifest data and all three raw
  executable sizes/SHA-256, with redirects rejected. Manifest JSON whitespace
  is server serialization, not part of the artifact identity.
- Fresh isolated public v0.0.8 peer acceptance passed shell-file proof, 21 exact
  native uploads without Enter, reload/resume and 403227 bytes of large output.
  Selected pair: local prflx / remote host, UDP. This is public-origin/LAN
  acceptance, not cellular or physical-phone evidence.
- An actual public v0.0.5 supervisor/browser pairing fixture automatically reached
  healthy cached v0.0.8 with exact identity bytes and its nonzero persisted pairing
  digest retained; no fresh pairing capability was supplied. **Full upgrade
  acceptance remains incomplete**: the extended monitor omitted the original
  GUI owner PID, so safety checks rejected its browser helper before post-upgrade
  shell/Core-PID proof. No application failure was established; native v8 hash
  was not captured within that fixture. Cleanup confirmed no owned processes
  remained. Sanitized evidence: ignored
  `dist/verification/relay-upgrade/report-v8.json`.

- Inline PNG QR rendering requires a positive, fresh-ID Kitty graphics reply;
  terminal names/environment variables are not used as capability evidence.
  Plain/non-TTY output does not probe. The complete private URL and text fallback
  remain available, with exact cell requirements when the text QR cannot fit.
- PNGs retain a four-module quiet zone. Kitty receives only a column scale, so
  the image stays square rather than stretching with terminal cell proportions.
  Log updates leave the image alone; resize and exit use ID-scoped cleanup.
- Disposable-PTY acceptance verified duplicate capability replies, a real resize,
  just two image transmissions (initial/resize), and exact termios restoration.
- An owned GUI workstation ran installed Herdr App **0.1.14** against an isolated
  Core on private headless Weston/Xwayland. First-run help was disabled only in
  the fixture's `HERDR_APP_HOME/shared.json` (`frame.welcomed: skipped`). The
  actual on-screen PNG was visually inspected and ZXing decoded its screenshot
  to the exact synthetic URL. The `.invalid` URL grants no access. This is desktop
  rendering/decoder evidence, **not physical phone-camera acceptance**.
- `make check` passed **310 tests** (plus one intentionally ignored interactive
  synthetic-preview fixture); `make release` and `make smoke` passed. Existing
  managed LAN 0.0.2 and public relay 0.0.5 were not restarted or replaced.

## Release provenance

The immutable `v0.0.3` tag (`3f908d6`) was not published: its Ubuntu 22.04 CI
caught an OpenSSL-dependent fixture serial filename; the macOS and Linux ARM jobs
passed. Version 0.0.4 explicitly chose the serial path, but OpenSSL 3.0 can omit
that optional artifact entirely. Version 0.0.5 allows the optional serial to be
absent while still requiring removal of signing keys, and adds a regression test.
Both failed tags remain unchanged and unpublished. This changes fixture portability/version metadata, not the native
transport design. Approved catalog publication still requires the complete
three-platform release pipeline.

## Published release and handoff

- **v0.0.5** published successfully by three-platform release CI **43524974192**.
  Immutable implementation tag: `e8d154204665691e233b4d98519c7b5b07660f28`.
- Fresh version 0.0.5 passed `make check`, `make release`, `make smoke`, direct,
  forced UDP and forced TCP acceptance. TLS passed on rerun after fixing a test
  race: a receipt observer can run before the UI clears upload busy state, so the
  helper now waits for the user-facing upload button before another picker.
  This is a test-helper follow-up, not a change to the published executable.
- `scripts/prepare-relay-release.py` authenticated to private GitHub, verified all
  three archives/checksums, safely unpacked the approved executables and probed the
  native version. Catalog: ignored `dist/relay-catalog/0.0.5/`; manifest SHA-256:
  `d49ab795b31cdbe782353033a767a3edebad309d3a181a53fc2d8a62d730ac0a`.
- The verified catalog was explicitly handed to the separate edge agent for its
  gated deployment. Public health, router/cellular traversal and physical phone
  keyboards are not established by this handoff. Managed LAN 0.0.2 remains live.

## Independent public endpoint/bootstrap verification

After the edge agent deployed the approved catalog, independent checks passed at
`https://herdr-webapp.perapp.dev`:

- Trusted HTTPS `/healthz`, `/run`, `/client/launcher.py`, and `/api/mode` reporting
  relay mode, version 0.0.5 and protocol 1. No redirects were accepted by the
  manifest/raw-asset verifier.
- Public manifest exactly equals the approved local catalog; all three public
  executable streams match approved CI size and SHA-256, not merely HTTP status.
- The actual public `/run` shell script ran with fresh private HOME/XDG directories,
  a disposable Core and no GitHub credentials. Verified native download, supervised
  0.0.5 startup, public WSS registration and Core readiness succeeded. The supervisor
  committed its current-version record. All private bootstrap state, children and
  Core were removed afterward.

At this initial bootstrap check, router forwarding still awaited user SSH
authentication. The subsequent LAN-hairpin acceptance below closes that specific
forwarding/peer-traffic gate, not cellular or physical-phone acceptance.
The original managed LAN 0.0.2 server still returns HTTP 200 and was not upgraded.

## Public relay / TURN LAN-hairpin peer acceptance

After the hosting agent configured WAN UDP/TCP 3478 and UDP 49160–49259 forwarding,
we ran the approved CI 0.0.5 executable against the real public HTTPS relay and
public TURN hostname from this LAN. Each phase used a fresh disposable Core,
shell, connector identity and owned isolated Chromium workstation; no user's Core
or terminals were used. No local relay/coturn or certificate exception was used.

The hosting agent temporarily advertised only UDP, then only TCP, retaining its
normal configuration snapshot and safety restore timer. Both browser and native
connector used relay-only ICE policy; the helper also asserted the advertised
TURN configuration contained only the requested transport, excluding a hidden
native UDP fallback in the TCP phase. Both phases passed:

- Selected candidates `relay`/`relay`; browser `relayProtocol=udp` or `tcp` as
  requested. Candidate protocol remains UDP: this is TURN-over-TCP, not ICE-TCP.
- Trusted browser input created independently checked native shell proof files.
- 21 distinct image files matched source bytes; uploads did not press Enter and
  all sequential uploads preserved the peer epoch.
- Browser reload and authenticated fresh-peer resume both produced shell proofs.
- 400,000 output characters crossed the credit window; each run observed 403,227
  terminal bytes and independently checked native shell completion.

All owned Cores, browser workstations, native children, pairing material and
private scratch state were cleaned up. Sanitized reports are ignored under
`dist/verification/relay-public-hairpin/`. Normal dual-transport restoration is
owned by the hosting agent, which confirmed byte-for-byte runtime-unit restoration,
public `/healthz` readiness, and removal of the temporary safety timer/snapshot.
This is **LAN hairpin** via
public hostnames, not external-network/cellular or physical iOS/Android proof.

## Automated checks

Latest `make check`, `make release` and `make smoke` completed successfully:

- 167 frontend tests, Svelte/TypeScript checks and ESLint.
- Rust formatting, warnings-as-errors Clippy, and 83 application Rust tests.
- Seven vendored TURN stream/TLS tests: framing, cancellation/close, malformed
  frames, certificate/name/IP verification, plaintext rejection and time bounds.
- 13 existing distribution tests, 10 relay-launcher tests, 10 catalog tests and
  twelve offline acceptance-harness tests (including external-mode isolation and
  mixed-transport rejection).
- Existing standalone local/federated smoke: durable authentication, exact Origin,
  topology/CAS/unknown-field preservation, native commands/uploads, terminal input,
  credit/resize/observer behavior, reconnect and logout. All against isolated Cores.

Launcher update/rollback tests use controlled offline executable fixtures. They do
not prove a genuine published three-platform relay-client upgrade.

## Real browser/native acceptance

An isolated rootless GUI workstation, disposable native Core and real compiled
connector/relay were used. No user's desktop was automated. Test capabilities and
TURN secrets were held only in private files and were deleted at cleanup.

Verified:

- QR-equivalent native pairing fragment, pinned P-256 identities, immediate URL
  fragment removal, real DTLS/SCTP DataChannels and selected ICE candidate stats.
- Direct route (`prflx`/`host`) and actual coturn UDP route (`relay`/`relay`).
- Earlier browser-TCP/native-UDP interoperability, followed by real two-sided
  native/browser **TCP** and **TLS** TURN acceptance after the local ICE patch.
  Both endpoints were forced to relay and given only the requested transport's
  URL. Selected pairs were `relay`/`relay`, with browser `relayProtocol` equal to
  `udp`, `tcp` or `tls`; candidate protocol remains UDP (not ICE-TCP).
- Native TLS used strict system-root/IP-SAN verification with a private fixture
  CA via fixture-only `SSL_CERT_FILE`; Python's TLS allocation probe also verified
  that CA/IP. Only the disposable Chromium used a certificate-error exception for
  its private CA. This is not proof of normal browser trust for a deployed cert.
- Each UDP/TCP/TLS run independently passed trusted shell-file input, 21 distinct
  byte-checked uploads with no Enter, full reload, authenticated reconnect/resume,
  and 400,000-byte output beyond the native credit window.
- Trusted browser keyboard input reached a real shell and created native proof
  files, not merely terminal echo or synthetic adapter assertions.
- Relay process restart: fresh peer epoch, resumed pairing and useful terminal.
- Full browser reload: persisted nonextractable browser key resumed successfully.
- Phone-sized 390×844 browser, direct/TURN input and output; this is **not physical
  iOS/Android keyboard validation**.
- Short TURN TTL (120 seconds): authenticated refresh/reconnection into new epochs
  without replaying pending application operations.
- 400,000 bytes of output traversed TURN beyond the native 262,144-byte credit
  window, with final output visible and shell completion independently verified.
- Native image bytes matched the generated fixture; original-gesture path paste
  did not press Enter. Twenty further sequential uploads preserved the same peer
  session beyond the former lifetime-channel limit. An additional upload passed
  over browser-TCP/native-UDP TURN.
- Real coturn rejected anonymous Allocate with a 401 authentication challenge.

Regression fixes discovered during integration include reactive Proxy/IndexedDB
serialization on reload, authorized-session lifetime, idle partial-frame expiry,
concurrent rather than lifetime channel accounting, control-ready publication,
aggregate send deadlines, and draining one-shot final responses without treating
clean sender EOF as failure. Connector state is exclusively locked independently
of the launcher's lock; readiness rechecks Core after relay registration.

Reusable acceptance is in `scripts/relay-e2e.py` and `scripts/relay-e2e/`; sanitized
reports live under ignored `dist/verification/relay-reusable/`. Earlier screenshots
and helpers remain in ignored `dist/verification/relay/`. Coturn uses rootless host
networking with a loopback-only listener and a finite LAN UDP allocation range,
not a second Pasta NAT. Its file capability requires retaining NET_BIND_SERVICE
in the container bounding set; other capabilities are dropped. Every owned Core,
relay/connector, coturn, GUI workstation and private secret is cleaned up.

Additional fixes include preserving the CDP observer across test reload sessions,
correct minimum-30-second TURN refresh scheduling, and bundled vendor license
notices (`herdr-webapp --licenses`, also included in archive LICENSE). The local
vendored source was independently compared against the registry archive: only the
five documented upstream files differ, plus the new adapter and patch notes.

## Explicit remaining gaps / deployment gates

1. Native TCP/TLS is implemented and locally verified, but does not add ICE-TCP,
   TURN-over-DTLS, HTTP proxies or virtual-network TCP support. Real restrictive
   networks still need public acceptance and correctly exposed TURN listeners.
2. Public HTTPS/bootstrap and LAN-hairpin UDP/TCP peer traffic passed against the
   deployed router/coturn. External-network/cellular/cross-NAT and physical phone
   keyboard testing are still outstanding.
3. Matching three-platform v0.0.8 release/catalog publication and deployment are
   verified. Published v0.0.5 → v0.0.8 supervisor activation and identity/pairing
   retention passed partial checks, but post-upgrade browser/Core-terminal proof
   was blocked by the verification fixture's GUI owner mismatch. Full published
   upgrade acceptance and failed-start rollback remain open; local offline
   supervisor fixtures are not substitutes.
4. Public enrollment is not operator-approved account admission. An actor owning
   both endpoint roles can obtain session TURN credentials; decide admission and
   finite quotas before exposing resources. Behind a proxy, backend socket-IP
   limits are shared proxy limits, not verified end-client IP limits.
5. Enforce correct coturn secret ownership and verify configuration actually loaded
   before publishing ports. The image's non-root default cannot read an unrelated
   host-owned 0600 mount; never resolve that by making the secret world-readable.
6. Controlled connector restarts and TURN refresh interrupt input briefly; in-place
   ICE renegotiation and continuous end-to-end-health rollback are not implemented.

The concrete edge/router handover is [`todo-edge.md`](todo-edge.md).
