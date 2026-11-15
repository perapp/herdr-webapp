# Installation and per-computer updates

## One setup command

Use the channel’s public command on the target computer, then scan the QR or
open its private pairing link. Stable and Main use separate connector state and
browser pairings; they share the computer’s Core.

```sh
curl -fsSL https://herdr-webapp.perapp.dev/run | sh
curl -fsSL https://herdr-webapp.perapp.dev/main/run | sh
```

The launcher probes the configured socket and existing installations first. It
reuses a compatible running Core without replacing its executable or restarting
terminals. A stopped compatible Core is started using `herdr-core server start`.
When Core is genuinely missing, setup announces the dependency, verifies the
stable release metadata’s protocol generation, pins that release, and invokes
the official `https://releases.herdr.dev/core/install.sh`. It then verifies and
starts Core before launching the connector. Main does **not** install a
bleeding-edge Core. The standalone webapp executable never installs dependencies.
The release installer uses the same bootstrap, including supported Core startup.

Setup handles its new executable’s PATH within that invocation; it does not edit
shell profiles. Required tools include curl, Python 3.9+, tar, and the official
installer’s checksum tools. Unsupported platform, network, permission, interrupted
installation and compatibility failures stop setup with recovery instructions.
Dependency installation uses a private lock; do not run another Core installer
concurrently. Inspect a stale lock before removing it after interruption.

### Managed and remote configurations

```sh
curl -fsSL https://herdr-webapp.perapp.dev/run | sh -s -- --no-install-core
curl -fsSL https://herdr-webapp.perapp.dev/main/run | sh -s -- --core-socket /path/to/core.sock
```

Explicit `HC_SOCKET`, `--core-socket`, configured Core sockets and `HC_SESSION`
are respected. An unavailable explicit endpoint never causes a new local Core
installation. Use an appropriate supported Unix-socket route for remote Core.
`--no-install-core` skips bootstrap entirely, including startup; it also works
with the release installer. Keep Core’s owner responsible for its updates:
Homebrew (`brew upgrade herdr-core`), mise (`mise upgrade herdr-core`), or Nix
configuration. An existing incompatible executable produces an explicit update
instruction rather than being overwritten during launch.

## Settings → Version

The popup separates these lifecycles:

- **Browser app:** checks the channel’s current hosted metadata. Update reloads
  only this browser window after an intentional action. Critical pairing, upload
  and input composition prevent reload; unsent drafts require confirmation.
- **Relay:** the hosting operator deploys it. A client cannot replace that service.
- **Connector:** controls the launcher on the selected computer and channel.
  Check only discovers a candidate. Update confirms that exact immutable release,
  downloads/verifies it, and performs a readiness-gated swap with rollback.
- **Local webapp host:** installer-managed local servers retain their authenticated
  release updater. Source builds and unsupported installations explain their
  original installation/update method.
- **Herdr Core:** manual checks offer only releases with the compatible protocol
  generation. Install delegates to official `hc update --json --version <checked>`;
  it never implements another binary replacement mechanism.

Connector checks default to every **30 seconds**, but installation is **manual**.
The launcher’s explicit `--auto-update` flag restores automatic connector upgrades.
Core checks are **manual only**. No frontend/connector upgrade automatically
updates Core. Local webapp release checks retain their existing startup/hourly
schedule; installation still requires confirmation.

In-popup Core execution is supported only when the selected **local Linux Core**
can be proven through same-UID Unix-socket kernel credentials, PID, running
executable and version. macOS, forwarded sockets and other unprovable mappings
show the official manual `hc update` instruction. Package-managed installations
always show their owner’s instruction, never direct replacement. An incompatible
latest generation is not offered; update clients deliberately before changing
protocol generations.

Authenticated selected-computer APIs accept only fixed component names and a
previously checked opaque release identity. No browser-provided command, URL or
executable path is executed. Duplicate/stale operations are rejected. The
launcher’s private same-user Unix socket is inherited by its connector; details
are in [the launcher contract](../distribution/relay-launcher.md).

Updates can interrupt the connection briefly. The browser reconnects normally,
never replays keystrokes or update mutations, and never opens the phone keyboard
without a gesture. Window-local progress survives connector teardown; ambiguous
outcomes ask the user to check actual status rather than claiming success.
Closing the launcher does not close Core’s terminals.

## Verification boundaries

Focused tests cover bootstrap reuse/start, opt-out, custom endpoints/PATH,
incompatibility/ownership, offline and interrupted setup, Stable/Main isolation,
launcher CAS/rollback and explicit-update policy, scoped backend auth/Origin,
Core peer credentials, duplicate requests, and frontend state/confirmation rules.
Disposable browser fixtures cover responsive appearance and update UI states;
real isolated Core/relay acceptance covers terminal continuity and reconnect.
Physical iOS/Android keyboard behavior and a real in-place production Core
upgrade remain separate acceptance boundaries; the user’s live Core is never
used for update testing.
