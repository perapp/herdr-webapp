# Per-relay launcher (no installation)

Public entry point: `curl -fsSL https://<relay>/run | sh`.
Requires Python **3.9+**, curl, and GNU Linux x86-64/arm64 or Apple Silicon macOS.
The existing global installer is independent and unchanged.

## Server contract

- `/run`: serve `run-relay.sh`, replacing **every** `__HERDR_RELAY_ORIGIN__`
  with the validated, canonical public HTTPS origin. Validate **before** string
  substitution: ASCII DNS name or bracketed IP, optional numeric port, no userinfo,
  quotes, backslashes, shell syntax, whitespace, path, query or fragment. Do not
  derive it from an untrusted Host/forwarded header. The shell's later validation
  is defense in depth, not a defense against unsafe template substitution.
- `/client/launcher.py`: serve `relay-launcher.py`. HTTPS is the bootstrap trust
  anchor. Do not redirect either endpoint; do not include access tokens.
- `/client/manifest.json`: JSON (at most 128 KiB):

  ```json
  {"schemaVersion":1,"protocolVersion":1,"version":"0.0.5","assets":[
    {"target":"x86_64-unknown-linux-gnu",
     "url":"/client/0.0.5/x86_64-unknown-linux-gnu/herdr-webapp",
     "sha256":"<64 lowercase hexadecimal characters>","size":12345678}
  ]}
  ```

  Other supported targets: `aarch64-unknown-linux-gnu`,
  `aarch64-apple-darwin`. Artifacts are **raw executables**, not archives.
  Exactly one matching target, stable `x.y.z` versions, protocol/schema exactly
  integer `1`, size 1..256 MiB. Paths must start `/client/`, contain only ASCII
  alphanumerics and `._/-`, with no `..` components. No redirects or cross-origin
  downloads. HTTPS plus the manifest checksum authenticates downloads; this is
  not an independent signature or protection against a compromised relay.
- Artifacts must be immutable at a given version. Publish verified artifacts
  before advertising them. The launcher fails closed on cached hash mismatch.

## Connector contract

A verified executable's `--version` must exit zero and print exactly
`herdr-webapp <version>` (trailing whitespace allowed, bounded output).
Then the supervisor launches, inheriting terminal stdio:

```
<cached-executable> connect-relay --relay-url <canonical-origin> \
  --state-dir <private-persistent-directory> --ready-file <unique-private-path>
```

No token is passed on argv. The connector stores pairing material in `state-dir`
and gracefully exits on SIGTERM. It must atomically replace `ready-file` with a
regular, single-link, user-owned **0600** JSON file only after healthy connection:

```json
{"schemaVersion":1,"pid":123,"version":"0.0.5","protocolVersion":1,"ready":true}
```

The PID must equal the directly launched child. Readiness is not a durable PID
file or permission to kill a process; only live `Popen` children are signalled.
Readiness files are removed after checking. Healthy startup defaults to 20s.

## Lifecycle and local files

Cache: `${XDG_CACHE_HOME:-~/.cache}/herdr-webapp/relay/<origin-hash>/`.
Pairing state: `${XDG_STATE_HOME:-~/.local/state}/herdr-webapp/relay/<origin-hash>/`.
Both are private 0700 directories; files must be user-owned, single-link and
private. Symlink directory components are rejected. A nonblocking file lock in
state serializes supervisors for the same canonical origin, even with different
cache roots. Nothing is added to PATH, shell profiles, autostart, or global config.
Stopping the launcher reaps its child, but keeps pairing state and verified cache.
Deleting cache does not delete state. Explicitly deleting state forgets pairing.
The origin is never a secret URL: origins with credentials/query/path are rejected.

Manifest polling defaults to 30s. Downloads and `--version` probes happen while
the old connector remains running. Newer compatible versions only; a readiness
failure stops that exact child and restarts the previous verified version using
the same state. A failed version is suppressed for the supervisor's lifetime;
a fresh invocation retries it. A fresh invocation also falls back to the last
healthy cached version if discovery/download/startup fails; an explicitly
incompatible manifest schema/protocol blocks a fresh launch instead. Cached fallback does
not imply the relay is online: readiness still must succeed. At most current and
previous executable are retained after a transition (a download temporarily adds
one); restarts may trim to just current. Pairing files are never pruned.

Interrupt/termination signals are forwarded through cleanup; normal child exit
status or terminating signal is propagated. No daemonization or persistent job is
created. The shell restores `/dev/tty` stdin when available, otherwise `/dev/null`.
This preserves terminal input in ordinary `curl | sh` use, but cannot promise
terminal job-control behavior for every shell/embedding. SIGKILL and machine
crashes cannot be cleaned up synchronously; abandoned download/save temporaries
are removed under the lock next run. Same-user hostile code and writable ancestors
raced by the same user are outside the file-ownership protection boundary.

There is no pairing-schema migration/backup mechanism here: connector releases
must preserve backward-compatible pairing state for rollback. Readiness validates
initial health only, not ongoing application behavior. An already-ready connector
that subsequently exits ends the supervisor; it is not endlessly restarted.

Development only: invoke Python directly with
`--relay-url http://127.0.0.1:<port> --allow-http-loopback`. The public shell never
allows HTTP. Optional `--cache-dir`/`--state-dir` are private root overrides;
`--poll-interval` 0.1..3600, `--ready-timeout` 0.1..120, and
`--network-timeout` 0.1..60 are bounded seconds (sub-second values are for tests).
Network requests have socket timeouts and a total streaming bound of four times
network timeout, checked between bounded reads. Environment proxies are ignored.

Offline verification:

```
python3 -B distribution/test_relay_launcher.py
```

Tests use isolated temporary files, a loopback fixture relay, and real tiny child
executables; no live core, public release, browser, or persistent server is used.
