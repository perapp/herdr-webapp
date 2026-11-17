# Enterprise deployment with Okta

An optional mode for a company-hosted relay. When it is enabled, **every** path
to a computer through that relay needs Okta authorization, and only the person
who enrolled a computer can open it. The ordinary deployment is unchanged when
the mode is not enabled.

This document describes what the software enforces. It is not a claim that the
result is equivalent to a VPN policy, and it does not replace a security review:
Okta sign-on policy, device posture and network controls are yours to decide.

## What is enforced

| Step | Who checks | What |
| --- | --- | --- |
| Connector start | connector, Okta | The owner signs in with the Okta device flow. The computer's persistent key is bound to that user (issuer + `sub`). |
| Connector registration | relay | A current Okta ID token for this application; the relay records the owner. Repeated at least every `recheck_seconds`. |
| Browser opens the relay | relay, Okta | Authorization Code + PKCE sign-in; the relay keeps a server-side session and re-verifies it with Okta at least every `recheck_seconds`. |
| Browser joins a computer | relay | The session's user must equal the computer's owner. Otherwise no signaling, no TURN credentials, and the connector is never contacted. |
| Session start | connector | A relay-signed grant naming the user, computer, browser key, relay and signaling session. The connector compares the user with **its own** enrolled owner before any Core operation. |
| While connected | relay, connector | Grants last `grant_lifetime_seconds` and are renewed at half life only while every check above still passes. Without renewal the connector closes the peer connection, including a direct WebRTC one. |

Same-user access is an invariant. There is no setting to share a computer.

The pairing QR still identifies and pins the computer and enrolls a browser key.
It never substitutes for sign-in: a QR, a previously paired browser key, a LAN
token or a hand-written signaling client gets nothing without the owner's
current Okta session. The existing key pinning and encrypted WebRTC transport
are unchanged.

## Okta application

Create **one** OIDC application and use it for both flows, so both ends see the
same issuer and the same `sub` for a person. Do not use two applications.

- Type: **Native Application** (public client, PKCE). A client secret is optional.
- Grant types: **Authorization Code**, **Refresh Token**, **Device Authorization**.
- Sign-in redirect URI: `https://<relay-host>/enterprise/callback`
- Scopes used: `openid profile offline_access`.
- Assign the people or groups allowed to use the relay. Unassigned users cannot
  sign in; removing an assignment or deactivating a user revokes their tokens.
- Attach the sign-on policy you require (MFA, re-authentication frequency).
- Issuer: the org authorization server (`https://<org>.okta.com`) or a custom
  one (`https://<org>.okta.com/oauth2/<id>`). ID tokens must be RS256.

The software can additionally *require proof* of what Okta enforced:
`required_amr = ["mfa"]` rejects ID tokens whose `amr` claim lacks `mfa`, and
`acr_values` is requested at sign-in and required in the `acr` claim. These
checks read validated token claims; they do not create an MFA requirement —
that is Okta policy. The device flow must satisfy the same requirements.

## Configuration

Configuration is discovered automatically. No command-line argument is needed.

| Layer | Path |
| --- | --- |
| System policy | `/etc/herdr-webapp/config.toml` (macOS: `/Library/Application Support/herdr-webapp/config.toml`) |
| User | `$XDG_CONFIG_HOME/herdr-webapp/config.toml`, default `~/.config/herdr-webapp/config.toml` |
| Explicit | `--config FILE` or `HERDR_WEBAPP_CONFIG`; replaces the user layer only and must exist |

Precedence: an **enabled system policy is authoritative**. Another layer may
repeat it exactly; any difference, including `enabled = false`, stops startup.
Without an enabled system policy the user (or explicit) layer decides.

```toml
[enterprise]
enabled = true
relay_url = "https://herdr.corp.example"     # the relay's public origin
issuer = "https://corp.okta.com"
client_id = "0oa…"

# Optional
client_secret_file = "/etc/herdr-webapp/okta-client-secret"  # relay only; mode 0600
revocations_file = "/etc/herdr-webapp/revocations.toml"      # relay only
required_amr = ["mfa"]
acr_values = "urn:okta:loa:2fa:any"
grant_lifetime_seconds = 300     # 30..900
recheck_seconds = 300            # 30..3600
session_max_seconds = 43200      # browser must sign in again after this
```

Files must be owned by root or the running user and not writable by group or
others. Unknown keys in `[enterprise]`, missing required values, non-HTTPS URLs,
out-of-range lifetimes, an unreadable secret or revocation file, a failed Okta
discovery, or a relay origin that differs from `--public-origin` all **stop
startup**. Nothing falls back to the non-Okta mode.

Secrets stay out of configuration and arguments: the optional client secret is
read from a private file by the relay and is never sent to a browser or a
connector. Deployment policy (these files), connector enrollment
(`<state>/enterprise.json`) and browser keys (IndexedDB) are stored separately.

### Relay

```sh
herdr-webapp serve-relay --bind 0.0.0.0 --port 8080    # origin and Okta settings come from the file
```

Terminate TLS in front of it (load balancer or reverse proxy) with the exact
origin from `relay_url`. `--enable-main` is refused in enterprise mode.

### Connector (the computer you want to reach)

```sh
herdr-webapp connect-relay --relay-url https://herdr.corp.example
```

The connector asks the relay which mode it enforces and adopts it, so a
computer needs no local file. If a local or system policy exists it **pins** the
issuer and client: a relay that does not enforce exactly that is refused.

On first start it prints an Okta address and a short code; open it on any
device and sign in as yourself. That user becomes the computer's owner. When
output is not a terminal the code is written to the private file
`<state>/enterprise-login.json` instead of the log. Later starts reuse the
stored enrollment silently. To hand the computer to someone else, start once
with `--reset-pairings` (this also revokes every paired browser).

The convenience launcher (`curl …/run | sh`) waits only briefly for readiness;
for the first, interactive enrollment start `connect-relay` directly.

With enterprise policy present on a computer, direct LAN mode (`herdr-webapp`
without a subcommand) refuses to start: it authenticates with a token, not Okta.

### Phone or browser

Scan the QR (or open the private link). The page keeps the pairing link in that
tab's `sessionStorage`, signs you in with Okta, and continues. The link is never
sent to the relay or to Okta and is discarded after ten minutes; if sign-in takes
longer you are asked to scan again. Later visits need only the Okta session.

## Trust and administrative boundaries

- **The relay is the policy authority.** It refuses connectors and browsers that
  do not present valid Okta authorization, regardless of what they run or how
  they are configured. This is what "cannot be bypassed" rests on.
- Local configuration constrains honest installations: it prevents accidental
  downgrade and pins the identity provider. It cannot constrain a person who
  controls their own computer; they could run other software entirely. Use
  endpoint management if you need that.
- **The relay origin, its frontend and its release pipeline are trusted.** The
  relay serves the JavaScript that holds browser keys and terminal content, and
  it signs grants. End-to-end encryption does not protect against malicious code
  served by a compromised relay or publisher. Operate it accordingly.
- The connector does not rely on the relay for *who the owner is*: it verifies
  its own Okta sign-in and refuses grants for any other user.

## Entry points

| Entry point | Before sign-in | Requires |
| --- | --- | --- |
| `/`, static assets, `/launch`, `/connections/<id>` | served | nothing; application shell only |
| `/api/mode`, `/healthz` | served | nothing; public metadata |
| `/run`, `/client/…` | served | nothing; public installer and binaries |
| `/enterprise/login`, `/enterprise/callback` | served | the sign-in itself (state, nonce, PKCE, browser binding) |
| `/enterprise/session` | 401 | Okta session cookie |
| `/enterprise/logout` | 403 | session cookie and exact `Origin` |
| `/relay/socket` `register`, `renew` | refused | connector key signature **and** a current owner ID token |
| `/relay/socket` `join` | refused | exact `Origin`, Okta session, owner match, no revocation |
| `/relay/socket` `signal`, `authorize` (TURN) | refused | an admitted session |
| WebRTC control/terminal/upload channels | refused | a verified grant, then the existing key authentication |
| `/main/…` | disabled | not available in enterprise mode |
| Connector listeners | none | the connector only connects outward |
| Direct LAN server | does not start | — |

## Revocation and its bounds

| Event | Effect | Bound |
| --- | --- | --- |
| User signs out (`Sign out`) | that browser's sessions end | about 5 s |
| Entry added to `revocations_file` | matching sessions, connectors and sign-ins refused | about 10 s |
| Okta deactivates the user, revokes their tokens, or unassigns the app | browser sessions end; the connector stops and exits | `recheck_seconds`, plus up to `grant_lifetime_seconds` if the relay itself is unreachable |
| Okta unreachable | no new grants; active sessions end | `recheck_seconds` + `grant_lifetime_seconds` |
| Relay unreachable from the connector | all sessions on that connector end | immediately on disconnect |
| `session_max_seconds` reached | browser must sign in again | at the next 5 s sweep |

Detection of Okta-side changes works by refreshing the user's tokens; it is as
prompt as Okta's revocation of refresh tokens and never faster than the recheck
interval. It is **not** immediate. With the defaults a revoked user loses access
within ten minutes. Lower `recheck_seconds` and `grant_lifetime_seconds` for a
tighter bound at the cost of more Okta token requests.

`revocations_file` (relay, reloaded when it changes):

```toml
subjects = ["00u1abcd…"]           # Okta user IDs (`sub`)
machines = ["<machineId>"]         # one computer
browser_keys = ["<browser public key>"]
```

A malformed list denies everyone until it is fixed. An owner can still revoke
all *other* browsers of a computer with **Settings → Rotate private access**.

An outage shows "Sign-in could not be verified"; access returns by itself when
Okta is reachable again. Nothing typed while disconnected is replayed.

## Audit

The relay writes JSON lines to standard error (`"audit":"herdr-enterprise"`);
the connector writes them to `<state>/enterprise-audit.log` (mode 0600, rotated
at 4 MiB). Events: `enrollment`, `connector-registered`, `connector-removed`,
`login`, `login-failed`, `logout`, `access-granted`, `access-denied`,
`grant-renewed`, `authorization-renewed`, `authorization-renewal-failed`,
`authorization-revoked`, `session-start`, `session-end`, `session-terminated`,
`revocations-loaded`, `revocations-unreadable`.

Records carry the Okta `sub`, the `machineId`, and short hashes of the session
and browser key for correlation. Tokens, cookies, pairing links, device codes
and terminal content are never logged.

## Limits

- One relay process holds the Okta sessions in memory: restarting it signs
  everyone out, and running several instances needs sticky routing.
- Quotas are unchanged (rooms, sessions, connections per address, sign-in
  attempts). TURN credentials are issued only for admitted sessions.
- The main channel is unavailable. Web Push and updates behave as before.
- Verified with a stand-in OpenID Connect provider and headless Chrome
  (`make enterprise-e2e`). A real Okta tenant and physical phones must be
  verified separately before relying on this.
