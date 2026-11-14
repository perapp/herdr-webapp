# Herdr Web

Use your Herdr terminals from a phone or desktop browser. One executable embeds
the frontend; Herdr Core owns the terminals, spaces, tabs and panes.

## Hosted access

Run on the computer already running **Herdr Core**. Requires curl and Python
3.9+, but no GitHub login. Scan the terminal QR with your phone to connect.

**Stable** — [open the app](https://herdr-webapp.perapp.dev/):

```sh
curl -fsSL https://herdr-webapp.perapp.dev/run | sh
```

**Main branch** — [open the development app](https://herdr-webapp.perapp.dev/main/):

```sh
curl -fsSL https://herdr-webapp.perapp.dev/main/run | sh
```

Stable and main keep separate pairings and client state. Main checks for new
commits every five minutes; the launcher follows verified connector updates.

Stable supports GNU/Linux x86-64/ARM64 and Apple Silicon macOS. **Hosted main
currently provides GNU/Linux x86-64 builds requiring glibc 2.39+.** For other
supported platforms, [build a matching main connector](docs/usage.md#build-a-main-connector-locally-including-macos-arm64--linux-arm64).

**QR links grant full shell access. Keep them private.** Connections use encrypted
WebRTC; the hosting origin is trusted to supply the application. Stopping the
connector does not stop Core or its terminals.

## Local / LAN

[Install a release](docs/usage.md#install-a-release), then:

```sh
herdr-webapp --port 8001
```

Scan the private LAN QR. Use a trusted LAN/VPN or HTTPS; do not expose plain HTTP
to the Internet.

## Build

Requires Rust 1.90+, Node.js 22.12+ (or 24+), npm, Make and a native C compiler.
Herdr Core must already be running.

```sh
make release
./target/release/herdr-webapp
```

## Documentation

- [Usage, installation and development](docs/usage.md)
- [Self-hosting stable and main](distribution/main-channel.md)
- [Connector launcher and updates](distribution/relay-launcher.md)
- [Release catalog](distribution/relay-catalog.md) · [Relay protocol](RELAY-PROTOCOL.md)
- [Verification results](RELAY-VERIFICATION.md)

Checks: `make check && make release && make smoke` (uses isolated test Cores).
