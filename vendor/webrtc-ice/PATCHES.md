# Local webrtc-ice patch

## Upstream provenance and licensing

- Package: **webrtc-ice 0.17.3**, upstream webrtc-rs ICE implementation.
- Archive: `https://static.crates.io/crates/webrtc-ice/webrtc-ice-0.17.3.crate`.
- Original registry archive SHA-256 (from the application's root `Cargo.lock`
  before replacing the registry dependency with this local package):
  `637ddc05fdc7d3da6471961433e18511c8544abd19c7519dac0899fde2ff84ec`.
- The archive was freshly fetched over HTTPS and its SHA-256 matched the lockfile
  before extraction into this directory. The independently cached archive had
  the same checksum; source files were **not copied from the registry cache**.
- The original MIT and Apache-2.0 license files are retained unchanged. Local
  changes are offered under the same dual license. `Cargo.lock` in this directory
  is the unchanged published artifact, not the application's active lockfile.

## Changes

Modified upstream files are annotated with local-extension comments:

- `Cargo.toml` / `Cargo.toml.orig`: add tokio-rustls 0.26 without default features,
  explicitly enabling `ring` and `tls12`; rustls-native-certs 0.8; rcgen 0.13 for
  TLS fixture tests.
- `src/lib.rs`: register the private `turn_stream` module.
- `src/agent/agent_gather.rs`: support native TCP connections to `turn:` servers
  and certificate-verified TLS connections to `turns:` servers when the URL's
  transport is TCP. Preserve UDP relay candidate semantics (this is not ICE-TCP).
  Pin TURN's destination to the connected socket address rather than resolving
  the hostname again. Explicitly close the underlying transport on construction,
  allocation and candidate failures. Bound DNS/connect/TLS negotiation to five
  seconds total and allocation to fifteen seconds. No downgrade or retry using
  plaintext/UDP follows a TLS failure.
- `src/candidate/candidate_base.rs`: retain and close the relay transport along
  with the client. Upstream TURN Client::close does not itself close that socket.

New `src/turn_stream.rs` implements the packet-oriented `util::Conn` interface
using bounded, independent reader and writer workers over an AsyncRead/AsyncWrite
stream. STUN uses its 20-byte header plus declared body; ChannelData uses its
4-byte header plus declared body and consumes/adds four-byte-aligned TCP padding.
There is no RFC 4571 length prefix. Frames are bounded by TURN's 65535-byte receive
buffer, including headers and wire padding. Malformed prefixes, STUN alignment,
cookie, oversize frames and truncated EOF are rejected. Frames are not truncated
to a caller's buffer. Writes are serialized and use write_all plus flush, including
short writes; cancellation of a send or recv does not corrupt the next frame.
Once a send is queued it may complete after its caller is cancelled. Close is
idempotent, wakes pending operations, and aborts both socket-owning workers; drop
also aborts workers. send_to cannot target a peer other than the connected server.

TLS uses the system trust store, explicit rustls ring provider, the original URL
hostname for ServerName (or IP SAN validation for IP literals), and normal chain
and name verification. No custom insecure verifier is installed.

## Verification and limitations

Unit tests in `turn_stream::tests` exercise split/coalesced STUN, ChannelData
padding, cancellation, concurrent short writes, malformed lengths/prefix/cookie,
EOF, destination checking, closure and blocked I/O; local rustls fixtures exercise
trusted hostname/IP certificates, wrong-host/untrusted/plaintext rejection, no
plaintext downgrade, refused connections and bounded stalled TLS negotiation.
Run from the application workspace with:

```sh
cargo test --locked -p webrtc-ice --lib turn_stream -j4
```

Virtual-network TCP/TLS is explicitly unsupported rather than bypassing vnet.
TURN-over-DTLS (`turns:...?transport=udp`), proxies, ICE-TCP, custom trust settings,
and custom client certificates are not added. TCP connection address selection
uses Tokio's native resolver; no Happy Eyeballs policy is introduced. Fixed
connect/allocation deadlines are not currently configurable.

Local adapter/TLS fixtures are not proof of native TURN interoperability. Native
TCP/TLS support must not be declared verified until application-level forced
relay tests against real coturn endpoints succeed, including both peers, followed
by the application's full check/release/smoke suite. Physical device keyboard
behavior is outside this transport patch's test scope.
