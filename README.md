# Herdr Web

Use your Herdr terminals from a phone or desktop browser. One executable embeds
the frontend; Herdr Core owns the terminals, spaces, tabs and panes.

## Hosted access

Run on the computer you want to use. Requires curl and Python 3.9+, but no
GitHub login. Missing compatible **Herdr Core** is installed through its official
installer and started; existing running Cores are reused without restart.
Scan the terminal QR with your phone to connect.

**Stable** — [open the app](https://herdr-webapp.perapp.dev/):

```sh
curl -fsSL https://herdr-webapp.perapp.dev/run | sh
```

**Main branch** — [open the development app](https://herdr-webapp.perapp.dev/main/):

```sh
curl -fsSL https://herdr-webapp.perapp.dev/main/run | sh
```

Stable and main keep separate pairings and client state. GitHub Actions builds
main for GNU/Linux x86-64/ARM64 and Apple Silicon macOS; the server checks for
complete verified builds every five minutes. The launcher checks for updates
every 30 seconds; install them explicitly in Settings → Version. Automatic
connector installation requires the launcher’s optional `--auto-update` flag.

Main Linux builds require glibc 2.35+ (x86-64) or 2.39+ (ARM64). See the
[usage guide](docs/usage.md#use-the-main-branch) for details and source builds.

**QR links grant full shell access. Keep them private.** Connections use encrypted
WebRTC; the hosting origin is trusted to supply the application. Stopping the
connector does not stop Core or its terminals.

Private links and browser authorization do not expire automatically. The terminal
hides its private link and QR after the first authenticated connection; press **L**
to toggle them. **Settings → Access → Rotate access token…** keeps this browser
profile connected while signing out other clients and invalidating old links for
that computer/channel. Browsers can still clear their own cookies or storage.

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

### Build versions

Clean builds at a version tag show that version number. Other builds show
`YYMMDDHHMMSS±HHMM-<7-character commit hash>`, using the commit’s timestamp and
timezone, not the build time. For example: `260603140506+0200-0123456`.
CLI `--build-version`, connection diagnostics and Settings → Version use this
identity; missing provenance is shown as unavailable, never guessed from the
package version. Legacy CLI `--version`, explicit `--package-version`, API mode
`version`, release manifests and update comparisons retain numeric package
identities so already-installed launchers and relay managers keep working.

## Documentation

- [Usage, installation and development](docs/usage.md)
- [Using it on a phone: gestures, keys, selection, attach](docs/phone.md)
- [Core bootstrap and per-computer updates](docs/installation-updates.md)
- [Installed apps and remembered computers](docs/pwa.md) · [Background notifications](docs/web-push.md)
- [Self-hosting stable and main](distribution/main-channel.md)
- [Connector launcher and updates](distribution/relay-launcher.md)
- [Release catalog](distribution/relay-catalog.md) · [Relay protocol](RELAY-PROTOCOL.md)
- [Verification results](RELAY-VERIFICATION.md)

Checks: `make check && make release && make smoke` (uses isolated test Cores).
