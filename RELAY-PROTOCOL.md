# Herdr relay protocol v1 — implementation contract

This document coordinates the relay implementation. Production acceptance and
limitations are tracked separately; this specification is not a claim of deployment.

## Modes and trust boundaries

- Existing no-subcommand CLI stays local/LAN compatible; `local` is an alias.
- `serve-relay --public-origin https://host ...` has no Core connection. It serves
  embedded assets, `/api/mode`, `/relay/socket`, `/client/manifest.json`, approved
  versioned binary downloads, `/client/launcher.py` and `/run`.
- `connect-relay --relay-url https://host --state-dir DIR --ready-file FILE ...`
  connects outbound and uses the configured local Core. No public laptop listener.
- Protocol integer is **1**, independently of release version. HTTPS/WSS required
  except an explicit development-only loopback HTTP option.
- Relay-delivered browser code is trusted. QR pinning protects against signaling
  substitution, not a compromised frontend or release publisher.

## Pairing and signaling

Use P-256 ECDSA SHA-256, raw SEC1 uncompressed 65-byte public keys and raw 64-byte
IEEE-P1363 signatures, encoded base64url without padding. Laptop identity is a
persistent 32-byte private key in an owner-only state directory. Its `machineId`
is base64url(SHA-256(public-key bytes)). Browser keys persist in IndexedDB using
non-extractable WebCrypto private CryptoKeys. The initial pairing capability is
32 random bytes, short-lived and consumed once; it is not the registration secret.

Pairing fragment contains JSON encoded base64url:

```
{v:1,machineId,publicKey,secret,expiresAt}
```

The containing URL provides the relay origin. Strip the fragment immediately;
never send `secret` in a relay request, logs, telemetry, referrer or SDP.

Signaling uses one same-origin WebSocket per participant, `/relay/socket`.
The relay sends `{type:"challenge",nonce:<random>}` first. Connector registration
is `{type:"register",machineId,publicKey,version,nonce,signature}`. Signature input:

```
herdr-register-v1\n<origin>\n<machineId>\n<relay-nonce>\n<version>
```

Verify public key hashes to machineId; reject duplicate active registrations.
Browser joins with `{type:"join",machineId}`. Relay allocates a fresh sessionId
and informs both endpoints using `{type:"session",sessionId,machineId}`.

Endpoint messages use `{type:"signal",sessionId,payload:<JSON string>}`. Relay
routes bounded opaque payloads only within that session. It must enforce bounded
rooms/sessions/messages, per-connection rate limits, join deadlines and cleanup.

Auth payload (browser first):

```
{kind:"auth",mode:"pair"|"resume",publicKey,nonce,proof}
```

Auth transcript input:

```
herdr-auth-v1\n<origin>\n<machineId>\n<sessionId>\n<browser-public-key>\n<browser-nonce>
```

Pair proof = HMAC-SHA256(capability, transcript). Resume proof = browser ECDSA
signature over the transcript. Laptop accepts only stored browser keys for resume.
Laptop responds with a signed envelope and then asks the relay to authorize this
session for short-lived TURN credentials. Pair capability is consumed and browser
key persisted only after authenticated WebRTC establishment, not on arbitrary join.

All post-auth endpoint payloads are signed envelopes:

```
{kind:"signed",seq:<positive safe integer>,body:<JSON string>,signature}
```

Signature input (exact body bytes, never parse/reserialize before verification):

```
herdr-signal-v1\n<origin>\n<machineId>\n<sessionId>\n<seq>\n<body>
```

Separate monotonic incoming/outgoing sequences per session. Reject replays,
unknown keys, duplicate auth, unexpected message ordering or protocol mismatch.
Laptop's first signed body is `{kind:"accepted",version,protocolVersion:1}`.
Browser sends its signed `{kind:"offer",sdp}`; laptop answers signed
`{kind:"answer",sdp}`. Full gathered SDP includes ICE candidates, so v1 need not
implement trickle ICE. Signatures bind DTLS fingerprints in SDP to the pinned keys.
SDP/candidate material must never be included in ordinary logs.

Relay authorization message from registered connector:
`{type:"authorize",sessionId}`. Relay emits
`{type:"ice",sessionId,iceServers:[{urls:[...],username,credential}],expiresAt}` to both.
`expiresAt` is Unix seconds. Direct sessions live while their endpoints remain
connected; only incomplete authorization has a short join deadline. A TURN-selected
browser reconnects into a fresh authenticated session before credential expiry
(ten percent of the remaining lifetime early, capped at sixty seconds, with a
one-second minimum margin). The minimum 30-second TTL does not reconnect every
second. In-place ICE renegotiation is not implemented;
this refresh causes a brief interruption without replaying operations.
TURN REST credentials use a private shared secret, finite expiry and quotas;
never expose the shared secret. Unauthenticated random joins do not receive them.
Public-key enrollment is public, however: controlling both endpoint roles permits
obtaining session credentials. This is not an operator-approved account/admission
system and must be covered by quotas/admission policy when deployed.

The local `webrtc-ice 0.17.3` patch adds native TURN TCP and verified TLS alongside
UDP. Both peers have passed forced-relay coturn acceptance with only the selected
transport's URL, including native TLS with a trusted private fixture CA. Native
TLS verifies the original hostname/IP SAN against system roots; there is no
insecure verifier or plaintext downgrade. These are UDP relay allocations over
TCP/TLS, not ICE-TCP/RFC6062. Virtual-network TCP, HTTP proxies, TURN-over-DTLS and
application-specific trust/client-certificate flags are not added. See
`vendor/webrtc-ice/PATCHES.md` for provenance, bounds, cleanup and limitations.

On participant loss, relay emits `{type:"ended",sessionId}` and forgets routes.
Reconnect gets a fresh sessionId and sequence space. No application operations
are sent over signaling. Re-establish WebRTC rather than replaying application data.

## DataChannels and application adapter

The browser opens reliable ordered `herdr-control-v1`. Connector accepts channels
only after endpoint authentication. Initial control hello identifies protocol and
session; no Core operation is allowed until it matches the authenticated session.

Control messages are bounded JSON:

- `{type:"request",id,method:"GET"|"POST",path,body?:<base64>,contentType?:...}`
- `{type:"response",id,status,body:<base64>}`
- `{type:"subscribe",id,path}` / `{type:"event",id,event,data}`
- `{type:"cancel",id}` / `{type:"closed",id}`

Use a fixed allowlist matching existing application routes. Do not accept raw
Core methods, arbitrary HTTP headers, arbitrary destinations or filesystem paths.
Attachments use a dedicated transfer channel rather than a huge control message.
The adapter reuses existing operation validation and native terminal semantics.

Every DataChannel application message uses binary fragmentation frames of at most
16,384 bytes. Header is 17 bytes: magic `[72,68,82,1]`, u32LE message ID at offset 4,
u32LE total byte count at 8, u32LE fragment offset at 12, kind byte at 16 (0 UTF-8
text, 1 binary), then payload. IDs are positive monotonic per channel; offsets must
be contiguous, metadata consistent, maximum logical size 16 MiB, at most four
partial messages and 32 MiB aggregate, with a 10-second partial timeout. Set lower
incoming logical limits per channel (terminal 64 KiB, upload chunk 64 KiB, control
2 MiB). Reject invalid/truncated/duplicate/replayed frames and never carry them to
a new channel/connection epoch. Bound SCTP buffered amount to 512 KiB and wait at
most two seconds for backpressure; terminal channels are separate from uploads.

Control channel starts with `{type:"hello",protocolVersion:1,sessionId}` and the
connector replies `{type:"ready",protocolVersion:1,version}`. Authenticate/consume
pair capability and persist browser key before sending ready or opening any Core
operation. The native peer layer handles hello; PeerService sees only subsequent
control messages.

Terminal DataChannels are named `herdr-terminal-v1:<id>` and start with an attach
JSON carrying the existing `/api/terminal` query fields. Subsequent text messages
are the existing terminal command JSON, and binary messages are raw terminal data.
Preserve existing initial output credit and parsed-byte acknowledgements. Split
outgoing Core data into <=16 KiB DataChannel messages and bound buffered output.
Do not copy 256 KiB Core frames as single SCTP messages.

The HTTP/WS/SSE frontend adapters must remain structurally compatible with existing
InputGate, connection generations and asynchronous upload/command checks. No
queued offline input or replayed mutations across connection epochs. Bytes already
accepted by a live transport cannot be recalled; use bounded buffers/liveness.

## Public release / launcher contract

`GET /client/manifest.json`:

```
{schemaVersion:1,protocolVersion:1,version:"x.y.z",assets:[
 {target,url:"/client/<version>/<target>/herdr-webapp",sha256,size}
]}
```

Only approved raw executable assets are public. Version is strict stable SemVer;
protocol must be supported. All asset URLs remain same-origin HTTPS, no redirects.
Launcher checks size/hash and executable `--version` before running verified bytes.
The current TLS origin is the bootstrap/update trust anchor; a separately protected
release-signing key would be additional hardening, not an implied existing feature.

The Python supervisor caches verified executables privately, maintains a lock per
relay/state identity, polls the compatible manifest, downloads while the old child
runs, then restarts only `connect-relay`. It preserves state and stdio, awaits a
private readiness marker, and rolls back failed startup with backoff/quarantine.
Connector readiness marker is atomic 0600 JSON:

```
{schemaVersion:1,pid,version,protocolVersion:1,ready:true}
```

Write readiness only after identity/state validation, successful Core handshake and
relay registration. Do not place credentials in argv or readiness files. Core is
never restarted. Stop connector children on supervisor shutdown; no orphan copies.
