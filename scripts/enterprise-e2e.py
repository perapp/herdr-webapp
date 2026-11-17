#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["playwright>=1.47,<2", "cryptography>=43", "websockets>=15,<16"]
# ///
"""Enterprise (Okta) mode end to end, with real binaries and a local stand-in IdP.

Starts an isolated Core, a relay configured only through its discovered
configuration file, a connector that adopts the relay's policy and enrolls with
the device flow, and headless phone/desktop Chrome. The identity provider is a
local RS256 OpenID Connect server: this proves the product's enforcement, not
Okta's own policies, and not behaviour on a physical phone.

Never uses the user's Core and never prints pairing links, codes or tokens.
"""
import argparse
import asyncio
import base64
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import parse_qs, urlencode, urlsplit

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ec, padding, rsa
from playwright.sync_api import sync_playwright
from websockets.asyncio.client import connect

PROJECT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('herdr_dev', PROJECT / 'scripts/dev.py')
dev = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dev)
VIS = '.kept-pane:not([hidden])'
CLIENT = '0oae2etestclient'
failures = []


def check(name, ok, detail=''):
    if not ok:
        failures.append(name)
    print(('PASS ' if ok else 'FAIL ') + name + (f'  [{detail}]' if detail and not ok else ''), flush=True)


def b64(data):
    return base64.urlsafe_b64encode(data).rstrip(b'=').decode()


class Idp:
    """Just enough OpenID Connect: discovery, JWKS, code + PKCE, device and refresh."""

    def __init__(self):
        self.key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        self.lock = threading.Lock()
        self.user = 'user-a'        # who the "interactive login" signs in
        self.deny = False           # the login page refuses
        self.outage = False         # token endpoint unavailable
        self.codes = {}             # code/device code -> (sub, nonce, challenge)
        self.refresh = {}           # refresh token -> sub
        self.seen = []              # every query/body the IdP received
        handler = self.handler()
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler)
        self.issuer = f'http://127.0.0.1:{self.server.server_address[1]}'
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def token(self, sub, nonce=None):
        now = int(time.time())
        claims = {'iss': self.issuer, 'aud': CLIENT, 'sub': sub, 'iat': now, 'exp': now + 3600,
                  'amr': ['pwd', 'mfa'], 'name': f'Test {sub}'}
        if nonce:
            claims['nonce'] = nonce
        head = b64(json.dumps({'alg': 'RS256', 'typ': 'JWT', 'kid': 'e2e'}).encode())
        body = b64(json.dumps(claims).encode())
        signature = self.key.sign(f'{head}.{body}'.encode(), padding.PKCS1v15(), hashes.SHA256())
        return f'{head}.{body}.{b64(signature)}'

    def revoke(self, sub):
        with self.lock:
            self.refresh = {token: owner for token, owner in self.refresh.items() if owner != sub}

    def handler(idp):
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def reply(self, status, body, headers=()):
                data = json.dumps(body).encode()
                self.send_response(status)
                self.send_header('content-type', 'application/json')
                self.send_header('content-length', str(len(data)))
                for name, value in headers:
                    self.send_header(name, value)
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self):
                url = urlsplit(self.path)
                query = {k: v[0] for k, v in parse_qs(url.query).items()}
                i = idp.issuer
                if url.path == '/.well-known/openid-configuration':
                    return self.reply(200, {'issuer': i, 'authorization_endpoint': f'{i}/authorize', 'token_endpoint': f'{i}/token',
                                            'jwks_uri': f'{i}/keys', 'device_authorization_endpoint': f'{i}/device',
                                            'id_token_signing_alg_values_supported': ['RS256'], 'code_challenge_methods_supported': ['S256']})
                if url.path == '/keys':
                    numbers = idp.key.public_key().public_numbers()
                    size = lambda n: n.to_bytes((n.bit_length() + 7) // 8, 'big')
                    return self.reply(200, {'keys': [{'kty': 'RSA', 'use': 'sig', 'alg': 'RS256', 'kid': 'e2e', 'n': b64(size(numbers.n)), 'e': b64(size(numbers.e))}]})
                if url.path == '/authorize':
                    with idp.lock:
                        idp.seen.append(self.path)
                        if query.get('client_id') != CLIENT or query.get('code_challenge_method') != 'S256':
                            return self.reply(400, {})
                        if idp.deny:
                            target = query['redirect_uri'] + '?' + urlencode({'error': 'access_denied', 'state': query['state']})
                        else:
                            code = secrets.token_urlsafe(24)
                            idp.codes[code] = (idp.user, query['nonce'], query['code_challenge'])
                            target = query['redirect_uri'] + '?' + urlencode({'code': code, 'state': query['state']})
                    self.send_response(302)
                    self.send_header('location', target)
                    self.send_header('content-length', '0')
                    self.end_headers()
                    return None
                return self.reply(404, {})

            def do_POST(self):
                form = {k: v[0] for k, v in parse_qs(self.rfile.read(int(self.headers.get('content-length', 0))).decode()).items()}
                with idp.lock:
                    idp.seen.append(json.dumps(sorted(form)))
                    if self.path == '/device':
                        code = secrets.token_urlsafe(24)
                        # The enrolling owner approves at once.
                        idp.codes[code] = (idp.user, None, None)
                        return self.reply(200, {'device_code': code, 'user_code': 'E2E-CODE', 'verification_uri': f'{idp.issuer}/activate',
                                                'interval': 1, 'expires_in': 120})
                    if self.path != '/token':
                        return self.reply(404, {})
                    if idp.outage:
                        return self.reply(503, {})
                    if form.get('client_id') != CLIENT:
                        return self.reply(400, {'error': 'invalid_client'})
                    kind = form.get('grant_type')
                    if kind == 'authorization_code':
                        sub, nonce, challenge = idp.codes.pop(form.get('code'), (None, None, None))
                        verifier = form.get('code_verifier', '')
                        if sub is None or b64(hashlib.sha256(verifier.encode()).digest()) != challenge:
                            return self.reply(400, {'error': 'invalid_grant'})
                    elif kind == 'urn:ietf:params:oauth:grant-type:device_code':
                        sub, nonce, _ = idp.codes.pop(form.get('device_code'), (None, None, None))
                        if sub is None:
                            return self.reply(400, {'error': 'expired_token'})
                    elif kind == 'refresh_token':
                        sub, nonce = idp.refresh.get(form.get('refresh_token')), None
                        if sub is None:
                            return self.reply(400, {'error': 'invalid_grant'})
                    else:
                        return self.reply(400, {'error': 'unsupported_grant_type'})
                    body = {'token_type': 'Bearer', 'id_token': idp.token(sub, nonce)}
                    if kind != 'refresh_token':
                        body['refresh_token'] = secrets.token_urlsafe(32)
                        idp.refresh[body['refresh_token']] = sub
                    return self.reply(200, body)
        return Handler


def free_port():
    import socket
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        return probe.getsockname()[1]


class Fixture:
    def __init__(self, binary, root):
        self.binary, self.root = str(binary), root
        self.hc = shutil.which('hc')
        if not self.hc:
            raise RuntimeError('hc must be installed')
        self.env = dev.isolated_env(root, os.environ)
        self.children = []
        self.idp = Idp()
        self.port = free_port()
        self.origin = f'http://127.0.0.1:{self.port}'
        # The relay gets its whole enterprise setup from the discovered file.
        self.relay_env = dict(self.env)
        relay_config = root / 'relay-config'
        (relay_config / 'herdr-webapp').mkdir(parents=True, mode=0o700)
        self.config = relay_config / 'herdr-webapp/config.toml'
        self.write_config(self.config)
        self.relay_env['XDG_CONFIG_HOME'] = str(relay_config)

    def write_config(self, path, extra='', enabled='true', issuer=None):
        path.write_text(f'[enterprise]\nenabled = {enabled}\nrelay_url = "{self.origin}"\nissuer = "{issuer or self.idp.issuer}"\n'
                        f'client_id = "{CLIENT}"\ngrant_lifetime_seconds = 30\nrecheck_seconds = 30\nrequired_amr = ["mfa"]\n{extra}')
        path.chmod(0o600)

    def hc_call(self, *args):
        return subprocess.run([self.hc, *args], env=self.env, cwd=self.root, capture_output=True, text=True, timeout=20, check=True).stdout

    def spawn(self, name, argv, env):
        log = open(self.root / f'{name}.log', 'wb')
        proc = subprocess.Popen([self.binary, *argv], env=env, cwd=self.root, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
        self.children.append(proc)
        return proc

    def exits(self, argv, env):
        """Run to completion; return (exit code, combined output)."""
        result = subprocess.run([self.binary, *argv], env=env, cwd=self.root, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)
        return result.returncode, result.stdout + result.stderr

    def start_core(self):
        self.hc_call('server', 'start')
        terminal = json.loads(self.hc_call('run', '--bg', '--name', 'e2e-shell', '--label', 'herdr.space=e2e-space', '--label', 'herdr.tab=e2e-tab',
                                           '/bin/bash', '--noprofile', '--norc'))['address']
        dev.Dev.rpc(self, 'doc.set', {'name': 'herdr-app', 'version': 0, 'value': {'spaces': [
            {'id': 'e2e-space', 'name': 'Enterprise', 'tabs': [{'id': 'e2e-tab', 'name': 'Terminal', 'root': {'id': 'e2e-root', 'type': 'row', 'children': [
                {'id': 'e2e-pane', 'type': 'block', 'content': {'terminal': terminal}}]}}]}]}})

    def start_relay(self):
        relay = self.spawn('relay', ['serve-relay', '--allow-http-loopback', '--bind', '127.0.0.1', '--port', str(self.port)], self.relay_env)
        self.wait_http('/healthz', relay)
        return relay

    def wait_http(self, path, proc, timeout=30):
        import urllib.request
        deadline = time.time() + timeout
        while time.time() < deadline:
            if proc.poll() is not None:
                raise RuntimeError('relay exited during startup')
            try:
                with urllib.request.urlopen(self.origin + path, timeout=1) as response:
                    return json.loads(response.read())
            except OSError:
                time.sleep(0.1)
        raise RuntimeError('relay did not become ready')

    def start_connector(self, state):
        ready = self.root / f'{state.name}.ready'
        proc = self.spawn(f'connector-{state.name}', ['connect-relay', '--allow-http-loopback', '--relay-url', self.origin, '--state-dir', str(state),
                                                      '--ready-file', str(ready), '--no-dashboard'], self.env)
        deadline = time.time() + 60
        while not ready.exists():
            if proc.poll() is not None or time.time() > deadline:
                raise RuntimeError('connector did not become ready')
            time.sleep(0.1)
        return proc

    def pairing_url(self, state):
        return f'{self.origin}/#pair={b64((state / "pairing.json").read_bytes())}'

    def close(self):
        for proc in reversed(self.children):
            if proc.poll() is None:
                dev.stop_group(proc)
        try:
            self.hc_call('server', 'stop')
        except (subprocess.SubprocessError, OSError):
            print('Warning: isolated Core stop failed.', file=sys.stderr)
        self.idp.server.shutdown()


def scope(page):
    """Phones keep panes mounted; the desktop layout has plain terminal hosts."""
    return VIS + ' ' if page.locator(VIS).count() else ''


def rows(page):
    return page.evaluate(f"[...document.querySelectorAll('{scope(page)}.xterm-rows > div')].map(d=>d.textContent).join('\\n')")


def shell(page, command, expect, timeout=15):
    page.locator(f'{scope(page)}.terminal-host').first.click()
    page.keyboard.type(command)
    page.keyboard.press('Enter')
    deadline = time.time() + timeout
    while time.time() < deadline:
        if expect in rows(page):
            return True
        time.sleep(0.1)
    return False


def new_page(context):
    context.add_init_script("try{localStorage.setItem('herdr.terminal-prefs','{\"gpu\":false}')}catch{}")
    page = context.new_page()
    page.errors = []
    page.on('pageerror', lambda error: page.errors.append(str(error)[:200]))
    return page


async def raw_join(origin, machine, cookie=None, with_origin=True):
    """A custom signaling client: what does the relay answer to a join?"""
    headers = {}
    if with_origin:
        headers['Origin'] = origin
    if cookie:
        headers['Cookie'] = cookie
    async with connect(origin.replace('http', 'ws', 1) + '/relay/socket', additional_headers=headers) as socket:
        await socket.recv()
        key = ec.generate_private_key(ec.SECP256R1()).public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)
        await socket.send(json.dumps({'type': 'join', 'machineId': machine, 'browserKey': b64(key)}))
        return json.loads(await asyncio.wait_for(socket.recv(), 10))


def join_as_custom_client(*args, **kwargs):
    # Playwright's sync API owns this thread's event loop.
    result = {}
    worker = threading.Thread(target=lambda: result.update(reply=asyncio.run(raw_join(*args, **kwargs))))
    worker.start()
    worker.join(30)
    return result.get('reply', {})


def startup_policy(f):
    """Fail-closed startup: none of these may run in a non-Okta mode."""
    code, output = f.exits(['--no-dashboard', '--port', str(free_port())], f.relay_env)
    check('direct LAN mode refuses to start under enterprise policy', code != 0 and 'enterprise' in output, code)
    code, output = f.exits(['serve-relay', '--allow-http-loopback', '--enable-main', '--bind', '127.0.0.1', '--port', str(free_port())], f.relay_env)
    check('relay refuses the main channel under enterprise policy', code != 0 and 'main channel' in output, code)
    broken = f.root / 'broken'
    (broken / 'herdr-webapp').mkdir(parents=True)
    for label, text in [('malformed', '[enterprise\n'), ('incomplete', '[enterprise]\nenabled = true\n'),
                        ('unknown setting', f'[enterprise]\nenabled = true\nrelay_url = "{f.origin}"\nissuer = "{f.idp.issuer}"\nclient_id = "{CLIENT}"\nrequire_same_user = false\n')]:
        (broken / 'herdr-webapp/config.toml').write_text(text)
        for mode in (['--no-dashboard', '--port', str(free_port())], ['serve-relay', '--allow-http-loopback', '--public-origin', f.origin, '--port', str(free_port())]):
            code, _ = f.exits(mode, f.relay_env | {'XDG_CONFIG_HOME': str(broken)})
            check(f'{label} configuration stops {"relay" if "serve-relay" in mode else "local"} startup', code != 0, code)
    unreachable = f.root / 'unreachable.toml'
    f.write_config(unreachable, issuer='http://127.0.0.1:9')
    code, _ = f.exits(['--config', str(unreachable), 'serve-relay', '--allow-http-loopback', '--port', str(free_port())], f.env)
    check('relay does not start when the identity provider cannot be verified', code != 0, code)
    code, _ = f.exits(['--config', str(f.root / 'absent.toml'), '--no-dashboard'], f.env)
    check('an explicit configuration file must exist', code != 0, code)
    # A connector whose own policy demands Okta refuses a relay that would not enforce it.
    other = f.root / 'other-idp.toml'
    f.write_config(other, issuer='http://127.0.0.1:10')
    code, output = f.exits(['--config', str(other), 'connect-relay', '--allow-http-loopback', '--state-dir', str(f.root / 'refused')], f.env)
    check('connector refuses a relay that enforces a different identity provider', code != 0 and 'refusing to connect' in output, code)


def run(f, playwright, browser):
    f.start_core()
    f.start_relay()
    mode = f.wait_http('/api/mode', f.children[-1])
    check('relay advertises enterprise mode from its discovered configuration', mode.get('enterprise', {}).get('required') is True and mode['publicOrigin'] == f.origin)
    startup_policy(f)

    state = f.root / 'connector-a'
    connector = f.start_connector(state)
    enrollment = json.loads((state / 'enterprise.json').read_text())
    check('connector adopted the relay policy and enrolled its owner through the device flow',
          enrollment['subject'] == 'user-a' and enrollment['issuer'] == f.idp.issuer and (state / 'enterprise.json').stat().st_mode & 0o077 == 0)
    pairing = json.loads((state / 'pairing.json').read_text())
    machine, secret = pairing['machineId'], pairing['secret']

    # Custom protocol clients: no Okta session, no terminal, whatever else they hold.
    reply = join_as_custom_client(f.origin, machine)
    check('custom client without sign-in is refused', reply.get('code') == 'enterprise-login-required', reply.get('type'))
    reply = join_as_custom_client(f.origin, machine, cookie='herdr-enterprise=' + 'A' * 43)
    check('custom client with an invented session cookie is refused', reply.get('code') == 'enterprise-login-required', reply.get('type'))
    reply = join_as_custom_client(f.origin, machine, with_origin=False)
    check('custom client without an Origin is refused', reply.get('type') == 'error', reply.get('type'))

    # Phone: scan the QR, sign in, land in the terminal.
    phone_context = browser.new_context(**playwright.devices['Pixel 7'])
    phone = new_page(phone_context)
    phone.goto(f.pairing_url(state))
    phone.wait_for_selector('.terminal-host .xterm', timeout=45000)
    check('phone: pairing link survives the sign-in redirect and opens the terminal', True)
    check('phone: terminal input works after sign-in', shell(phone, 'echo ENT_$((40+2))', 'ENT_42'))
    leaked = [entry for entry in f.idp.seen if secret in entry or 'pair' in entry.split('redirect_uri')[0].lower() and 'pair=' in entry]
    stash = phone.evaluate("Object.keys(sessionStorage).filter(k => k.includes('enterprise.pairing')).length")
    check('phone: pairing secret never left the browser and is not kept', not leaked and '#' not in phone.url and secret not in phone.url and stash == 0)
    cookies = {c['name']: c for c in phone_context.cookies()}
    session = cookies.get('herdr-enterprise', {})
    check('session cookie is HttpOnly and SameSite=Strict', session.get('httpOnly') is True and session.get('sameSite') == 'Strict', sorted(cookies))
    check('phone: account is not shown a foreign identity', phone.evaluate("document.body.innerText.includes('user-b')") is False)

    # Returning browser: stored key + valid session reconnects without pairing again.
    phone.reload()
    phone.wait_for_selector('.terminal-host .xterm', timeout=45000)
    check('phone: reconnect after reload resumes with a fresh grant', shell(phone, 'echo AGAIN_$((1+1))', 'AGAIN_2'))

    # The same stored browser key without the Okta session is worthless.
    saved = phone_context.cookies()
    phone_context.clear_cookies()
    f.idp.deny = True
    phone.reload()
    phone.wait_for_selector('[data-testid=enterprise-sign-in]', timeout=30000)
    check('phone: a paired browser without sign-in gets no terminal', phone.locator('.terminal-host').count() == 0)
    f.idp.deny = False
    phone_context.add_cookies(saved)
    phone.goto(f'{f.origin}/connections/{machine}')
    phone.wait_for_selector('.terminal-host .xterm', timeout=45000)

    # Desktop: another valid corporate user with the very same QR.
    f.idp.user = 'user-b'
    desktop_context = browser.new_context(viewport={'width': 1280, 'height': 800})
    desktop = new_page(desktop_context)
    desktop.goto(f.pairing_url(state))
    desktop.wait_for_selector('text=Not your computer', timeout=45000)
    check('desktop: another signed-in user holding the QR is refused', desktop.locator('.terminal-host').count() == 0)
    stranger = next(c for c in desktop_context.cookies() if c['name'] == 'herdr-enterprise')
    reply = join_as_custom_client(f.origin, machine, cookie=f'herdr-enterprise={stranger["value"]}')
    check('custom client with another user’s session is refused', reply.get('code') == 'enterprise-owner-mismatch', reply.get('type'))
    desktop_context.close()

    # Desktop as the owner: second browser of the same user works.
    f.idp.user = 'user-a'
    desktop_context = browser.new_context(viewport={'width': 1280, 'height': 800})
    desktop = new_page(desktop_context)
    desktop.goto(f.pairing_url(state))
    desktop.wait_for_selector('.terminal-host .xterm', timeout=45000)
    check('desktop: owner pairs a second browser and types', shell(desktop, 'echo DESK_$((2+3))', 'DESK_5'))

    # Sign-out ends that browser's established (direct WebRTC) session.
    desktop.evaluate("fetch('/enterprise/logout', {method: 'POST'})")
    try:
        desktop.wait_for_selector('[data-testid=enterprise-sign-in], [data-testid=connection-workspace]', timeout=30000)
        ended = True
    except Exception:
        ended = False
    check('desktop: sign-out terminates the active session', ended and desktop.locator('.terminal-host').count() == 0)
    desktop_context.close()
    check('phone: another browser signing out does not disturb this one', shell(phone, 'echo STILL_$((3+3))', 'STILL_6'))

    # Active session outlives several grant lifetimes only through renewal.
    time.sleep(40)
    check('phone: session continues across grant renewals', shell(phone, 'echo RENEW_$((4+4))', 'RENEW_8'))
    audit = (state / 'enterprise-audit.log').read_text()
    check('connector audit records grants, renewal and sessions without secrets',
          all(event in audit for event in ('"enrollment"', '"access-granted"', '"grant-renewed"', '"session-start"', '"session-end"')) and secret not in audit)

    # Identity provider outage: access closes within recheck + grant lifetime.
    f.idp.outage = True
    started = time.time()
    try:
        # The relay's CSP forbids evaluated polling scripts; wait on the DOM instead.
        phone.wait_for_selector('.terminal-host', state='detached', timeout=90000)
        closed = time.time() - started
    except Exception:
        closed = None
    check('outage: active session ends within the documented bound (60 s + sweep)', closed is not None and closed <= 75, closed)
    check('outage: connector stays up but offline', connector.poll() is None)
    f.idp.outage = False
    deadline = time.time() + 90
    recovered = False
    while time.time() < deadline and not recovered:
        phone.goto(f'{f.origin}/connections/{machine}')
        try:
            phone.wait_for_selector('.terminal-host .xterm', timeout=15000)
            recovered = True
        except Exception:
            pass
    check('outage: access returns after recovery without re-pairing', recovered and shell(phone, 'echo BACK_$((5+5))', 'BACK_10'))

    # Okta revokes the user: browser session and connector enrollment both end.
    f.idp.deny = True
    f.idp.revoke('user-a')
    started = time.time()
    try:
        phone.wait_for_selector('[data-testid=enterprise-sign-in]', timeout=90000)
        closed = time.time() - started
    except Exception:
        closed = None
    check('revocation: phone loses the terminal and is asked to sign in', closed is not None and closed <= 75, closed)
    try:
        code = connector.wait(timeout=60)
    except subprocess.TimeoutExpired:
        code = None
    check('revocation: connector stops serving and exits for re-enrollment', code not in (None, 0), code)
    relay_log = (f.root / 'relay.log').read_text(errors='replace')
    check('relay audit records sign-in, denial and termination without secrets',
          all(event in relay_log for event in ('"login"', '"access-denied"', '"access-granted"', '"session-terminated"', '"connector-registered"')) and secret not in relay_log)
    check('no page errors', not phone.errors, phone.errors[:2])
    phone_context.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=PROJECT / 'target/release/herdr-webapp')
    parser.add_argument('--keep', action='store_true', help='keep the private fixture directory for inspection')
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix='herdr-ent-e2e-'))
    root.chmod(0o700)
    fixture = Fixture(args.binary.resolve(), root)
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(channel='chrome', headless=True, args=['--disable-features=WebRtcHideLocalIpsWithMdns'])
            try:
                run(fixture, playwright, browser)
            finally:
                browser.close()
    finally:
        fixture.close()
        if args.keep:
            print(f'Kept PRIVATE fixture: {root}', file=sys.stderr)
        else:
            shutil.rmtree(root, ignore_errors=True)
    if failures:
        print(f'ENTERPRISE E2E FAILED: {len(failures)} check(s): ' + '; '.join(failures))
        return 1
    print('ALL ENTERPRISE E2E CHECKS PASSED (isolated Core, stand-in IdP, headless phone and desktop Chrome)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
