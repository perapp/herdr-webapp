# Relay implementation verification — source 0.0.5

This records implementation/local acceptance, **not public deployment approval**.
Tests used disposable Cores and owned GUI workstations; the live user Core and
managed 0.0.2 LAN server were left untouched. A separate edge agent owns actual
hosting/DNS/router deployment. Release publication is separate from these results.

## Release provenance

The immutable `v0.0.3` tag (`3f908d6`) was not published: its Ubuntu 22.04 CI
caught an OpenSSL-dependent fixture serial filename; the macOS and Linux ARM jobs
passed. Version 0.0.4 explicitly chose the serial path, but OpenSSL 3.0 can omit
that optional artifact entirely. Version 0.0.5 allows the optional serial to be
absent while still requiring removal of signing keys, and adds a regression test.
Both failed tags remain unchanged and unpublished. This changes fixture portability/version metadata, not the native
transport design. Approved catalog publication still requires the complete
three-platform release pipeline.

## Automated checks

Latest `make check`, `make release` and `make smoke` completed successfully:

- 167 frontend tests, Svelte/TypeScript checks and ESLint.
- Rust formatting, warnings-as-errors Clippy, and 83 application Rust tests.
- Seven vendored TURN stream/TLS tests: framing, cancellation/close, malformed
  frames, certificate/name/IP verification, plaintext rejection and time bounds.
- 13 existing distribution tests, 10 relay-launcher tests, 10 catalog tests and
  ten offline acceptance-harness tests.
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
2. No public/cellular/cross-NAT acceptance, real router/coturn deployment or
   physical phone keyboard testing has been performed in this task.
3. Publish/verify the matching three-platform release before advertising `/run`.
   Test actual published connector upgrade and failed-start rollback with retained
   pairing/Core terminals. Local offline supervisor fixtures are not that evidence.
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
