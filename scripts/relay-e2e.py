#!/usr/bin/env python3
"""Isolated real relay acceptance runner. Never emits captured process output."""
import argparse
import contextlib
import hashlib
import hmac
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
IMAGE = 'docker.io/coturn/coturn'
BROWSER_LABELS = {'observer installed', 'reload observer installed', 'terminal attached',
                  'peer connected', 'shell sentinel', 'acceptance condition',
                  'selected ICE pair missing', 'oversized SCTP frame', 'unexpected relay route',
                  'both peers must select relay', 'wrong TURN transport selected',
                  'upload reconnected peer', 'pairing fragment persisted', 'browser operation failed',
                  'upload ready', 'mixed TURN transport configuration'}


class Failure(Exception):
    pass


def require(value, label):
    if not value:
        raise Failure(label)


def private(path, data):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as stream:
        stream.write(data)


def run(args, *, env=None, cwd=None, timeout=60):
    result = subprocess.run(args, env=env, cwd=cwd, capture_output=True, timeout=timeout)
    require(result.returncode == 0, 'subprocess failed (private output suppressed)')
    return result.stdout


def turn_auth(port, transport, secret, realm, cafile=None):
    """Verify an actual 401 realm/nonce challenge, then a REST-authenticated Allocate."""
    import base64
    cookie = 0x2112A442

    def attribute(kind, value):
        return struct.pack('!HH', kind, len(value)) + value + b'\0' * (-len(value) % 4)

    def exchange(sock, attrs, key=None):
        tx = secrets.token_bytes(12)
        header = struct.pack('!HHI12s', 3, len(attrs) + (24 if key else 0), cookie, tx)
        packet = header + attrs
        if key:
            packet += attribute(8, hmac.new(key, packet, hashlib.sha1).digest())
        sock.sendall(packet)
        if transport in ('tcp', 'tls'):
            def exact(n):
                data = b''
                while len(data) < n:
                    part = sock.recv(n - len(data))
                    require(part, 'TURN probe truncated')
                    data += part
                return data
            response = exact(20)
            response += exact(struct.unpack('!H', response[2:4])[0])
        else:
            response = sock.recv(65535)
        require(len(response) >= 20 and response[8:20] == tx, 'TURN transaction mismatch')
        values = {}
        pos = 20
        while pos + 4 <= len(response):
            kind, size = struct.unpack('!HH', response[pos:pos + 4])
            values[kind] = response[pos + 4:pos + 4 + size]
            pos += 4 + size + (-size % 4)
        return struct.unpack('!H', response[:2])[0], values

    with contextlib.ExitStack() as stack:
        sock = stack.enter_context(socket.socket(socket.AF_INET,
                    socket.SOCK_STREAM if transport in ('tcp', 'tls') else socket.SOCK_DGRAM))
        sock.settimeout(5)
        if transport == 'tls':
            require(cafile is not None, 'TLS probe requires private CA')
            context = ssl.create_default_context(cafile=str(cafile))
            sock = stack.enter_context(context.wrap_socket(sock, server_hostname='127.0.0.1'))
        sock.connect(('127.0.0.1', port))
        requested = attribute(0x19, b'\x11\0\0\0')
        kind, values = exchange(sock, requested)
        require(kind == 0x113 and values.get(0x14) == realm.encode() and values.get(0x15)
                and values.get(9, b'')[2:4] == b'\x04\x01', 'TURN realm/auth challenge invalid')
        user = f'{int(time.time()) + 600}:relay-e2e'.encode()
        password = base64.b64encode(hmac.new(secret.encode(), user, hashlib.sha1).digest())
        key = hashlib.md5(user + b':' + realm.encode() + b':' + password).digest()
        kind, response = exchange(sock, requested + attribute(6, user) + attribute(0x14, realm.encode())
                           + attribute(0x15, values[0x15]), key)
        require(kind == 0x103, f'TURN REST authenticated allocation failed (type={kind:04x}, error={response.get(9, bytes(4))[2:4].hex()})')


class Harness:
    def __init__(self, args):
        self.args = args
        self.hc = shutil.which('hc')
        require(self.hc, 'hc executable missing')
        self.root = Path(tempfile.mkdtemp(prefix='herdr-relay-e2e-'))
        self.children = []
        self.gui = None
        self.container = None
        self.core_started = False
        self.phase = 'setup'
        self.env = {k: os.environ[k] for k in ('PATH', 'LANG', 'USER', 'LOGNAME', 'SHELL') if k in os.environ}
        for key, name in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'), ('XDG_STATE_HOME', 'state'),
                          ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache'), ('XDG_RUNTIME_DIR', 'run')]:
            path = self.root / name
            path.mkdir(mode=0o700)
            self.env[key] = str(path)
        self.env.update(HC_SOCKET=str(self.root / 'core.sock'), HC_SESSION='relay-e2e')
        self.gui_env = os.environ | {'GUI_WORKSTATION_UPGRADE_DAYS': '0',
                                    'GUI_WORKSTATION_NETWORK': 'pasta:-T,auto,-U,auto',
                                    'GUI_WORKSTATION_OWNER_PID': str(os.getpid())}
        self.workspace = self.root / 'browser'
        self.workspace.mkdir(mode=0o700)

    def gui_call(self, *args, timeout=120):
        prefix = [str(self.args.gui_cli)]
        if self.gui:
            prefix += ['--id', self.gui]
        return run(prefix + list(args), env=self.gui_env, timeout=timeout)

    def hc_call(self, *args):
        return run([self.hc, *args], env=self.env, cwd=self.root)

    def spawn(self, args, name):
        with open(self.root / (name + '.log'), 'ab') as log:
            proc = subprocess.Popen(args, env=self.env, cwd=self.root, stdin=subprocess.DEVNULL,
                                    stdout=log, stderr=log, start_new_session=True)
        self.children.append(proc)
        return proc

    def rpc(self, method, params):
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(10)
            sock.connect(self.env['HC_SOCKET'])
            with sock.makefile('rw') as stream:
                for index, (name, value) in enumerate([('hello', {'generation': 1}), (method, params)]):
                    stream.write(json.dumps({'jsonrpc': '2.0', 'id': index, 'method': name, 'params': value}) + '\n')
                    stream.flush()
                    reply = json.loads(stream.readline())
                    require('error' not in reply, 'isolated Core RPC failed')
                return reply['result']

    def start_turn(self):
        self.phase = 'TURN private preflight'
        require(run(['podman', 'info', '--format', '{{.Host.Security.Rootless}}']).strip() == b'true',
                'coturn requires rootless Podman')
        run(['podman', 'image', 'exists', IMAGE])  # Deliberately never pull.
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            # Routing lookup only: UDP connect sends no packet.
            sock.connect(('192.0.2.1', 9))
            advertised = sock.getsockname()[0]
        require(not advertised.startswith('127.'), 'no routable host IPv4 detected')
        port = self.free_port()
        # Reserve a contiguous finite range while selecting; Podman fails closed on races.
        for _ in range(100):
            low = 40000 + secrets.randbelow(1800) * 10
            reservations = []
            try:
                for p in range(low, low + 16):
                    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                    reservations.append(s)
                    s.bind(('0.0.0.0', p))
                break
            except OSError:
                pass
            finally:
                for s in reservations:
                    s.close()
        else:
            raise Failure('no free TURN relay range')
        secret = secrets.token_urlsafe(48)
        realm = 'relay-e2e.invalid'
        secret_path = self.root / 'turn-secret'
        private(secret_path, secret)
        turn_dir = self.root / 'turn'
        turn_dir.mkdir(mode=0o700)
        cafile = self.make_tls(turn_dir) if self.args.transport == 'tls' else None
        if cafile:
            # Fixture subprocesses only; native rustls still validates roots and IP SAN.
            self.env['SSL_CERT_FILE'] = str(cafile)
        config = turn_dir / 'turn.conf'
        listener = (f'tls-listening-port={port}\nno-tcp\nno-udp\n'
                    'cert=/private/leaf.pem\npkey=/private/leaf.key\n'
                    if cafile else f'listening-port={port}\nno-tls\n')
        private(config, listener + f'listening-ip=127.0.0.1\nrelay-ip={advertised}\n'
                f'min-port={low}\nmax-port={low + 15}\nrealm={realm}\nuse-auth-secret\n'
                f'static-auth-secret={secret}\nuser-quota=8\ntotal-quota=24\nmax-allocate-lifetime=600\n'
                'no-cli\nno-dtls\nno-multicast-peers\nno-loopback-peers\nlog-file=stdout\n')
        require(config.stat().st_mode & 0o777 == 0o600 and config.stat().st_uid == os.getuid(),
                'TURN configuration is not private')
        mount = f'{turn_dir}:/private:ro,Z'
        # Image defaults to an unprivileged UID: explicitly use namespace root for 0600 mount.
        run(['podman', 'run', '--rm', '--pull=never', '--user', '0', '--network', 'none',
             '-v', mount, '--entrypoint', '/bin/sh', IMAGE, '-ec',
             'test -r /private/turn.conf && test "$(stat -c %a /private/turn.conf)" = 600'
             + (' && test -r /private/leaf.pem && test -r /private/leaf.key'
                ' && test -r /private/ca.pem && test ! -e /private/ca.key' if cafile else '')])
        self.container = 'herdr-relay-e2e-' + secrets.token_hex(8)
        run(['podman', 'run', '-d', '--name', self.container, '--pull=never', '--user', '0',
             # Image's turnserver file capability must stay in the bounding set,
             # otherwise execve fails EPERM even with an unprivileged listener port.
             '--cap-drop=all', '--cap-add=NET_BIND_SERVICE', '--security-opt=no-new-privileges', '-v', mount,
             # Avoid a second Pasta NAT around the TURN allocation sockets.
             # Listener is loopback-only; relay sockets bind the finite LAN range.
             '--network=host', IMAGE,
             '-c', '/private/turn.conf'])
        for attempt in range(30):
            try:
                turn_auth(port, self.args.transport, secret, realm, cafile)
                break
            except (OSError, Failure) as error:
                if attempt == 29:
                    reason = str(error) if isinstance(error, Failure) else type(error).__name__
                    raise Failure('TURN realm/auth allocation preflight failed: ' + reason) from None
                time.sleep(.2)
        scheme = 'turns' if cafile else 'turn'
        protocol = 'tcp' if cafile else self.args.transport
        return ['--turn-secret-file', str(secret_path), '--turn-url',
                f'{scheme}:127.0.0.1:{port}?transport={protocol}']

    def make_tls(self, directory):
        """Private short-lived CA and IP-SAN leaf; no system trust changes."""
        ca = directory / 'ca.pem'
        run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
             '-keyout', str(directory / 'ca.key'), '-out', str(ca), '-subj', '/CN=relay-e2e CA',
             '-addext', 'basicConstraints=critical,CA:TRUE',
             '-addext', 'keyUsage=critical,keyCertSign,cRLSign'])
        run(['openssl', 'req', '-new', '-newkey', 'rsa:2048', '-nodes',
             '-keyout', str(directory / 'leaf.key'), '-out', str(directory / 'leaf.csr'),
             '-subj', '/CN=127.0.0.1'])
        extensions = directory / 'leaf.ext'
        private(extensions, 'subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\n'
                'keyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n')
        run(['openssl', 'x509', '-req', '-in', str(directory / 'leaf.csr'), '-CA', str(ca),
             '-CAkey', str(directory / 'ca.key'), '-CAserial', str(directory / 'ca.srl'),
             '-CAcreateserial', '-days', '1',
             '-extfile', str(extensions), '-out', str(directory / 'leaf.pem')])
        # Never expose CA signing material through the coturn mount.
        for name in ('ca.key', 'ca.srl', 'leaf.csr', 'leaf.ext'):
            # Some OpenSSL versions sign without creating the optional serial
            # artifact, even with -CAcreateserial. Signing keys remain mandatory.
            (directory / name).unlink(missing_ok=(name == 'ca.srl'))
        for path in directory.iterdir():
            path.chmod(0o600)
        return ca

    @staticmethod
    def free_port():
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            return sock.getsockname()[1]

    def browser(self, action):
        self.phase = action
        result_path = self.workspace / 'result.json'
        result_path.unlink(missing_ok=True)
        try:
            self.gui_call('exec', 'python3', '/workspace/browser.py', action, timeout=240)
        except Failure:
            if not result_path.is_file():
                raise
        result = json.loads(result_path.read_text())
        label = result.get('assertion')
        require(result.get('ok') is True,
                label if label in BROWSER_LABELS else 'browser acceptance failed')
        return result

    def execute(self):
        require(self.args.binary.is_file(), 'binary missing; build release first')
        # Explicit owner PID keeps acquire/release ownership stable across CLI subprocesses.
        acquired = json.loads(self.gui_call('acquire', '--workspace', str(self.workspace),
                                           '--owner-pid', str(os.getpid()), '--json'))
        self.gui = acquired['id']  # Never print returned noVNC credentials.
        shutil.copyfile(HERE / 'relay-e2e' / 'browser.py', self.workspace / 'browser.py')
        self.phase = 'isolated Core'
        self.core_started = True  # Also clean up a partially successful start.
        self.hc_call('server', 'start')
        terminal = json.loads(self.hc_call('run', '--bg', '--name', 'relay-e2e', '--label',
                    'herdr.space=e2e-space', '--label', 'herdr.tab=e2e-tab',
                    'bash', '--noprofile', '--norc'))['address']
        self.rpc('doc.set', {'name': 'herdr-app', 'version': 0, 'value': {'spaces': [
            {'id': 'e2e-space', 'name': 'Acceptance', 'tabs': [{'id': 'e2e-tab', 'name': 'Terminal',
             'root': {'id': 'e2e-root', 'type': 'row', 'children': [{'id': 'e2e-pane', 'type': 'block',
             'content': {'terminal': terminal}}]}}]}], 'acceptanceExtension': {'preserve': True}}})
        external = self.args.relay_origin
        force_turn = self.args.transport != 'direct'
        self.phase = 'relay readiness'
        origin = external or f'http://127.0.0.1:{self.args.port}'
        if not external:
            turn = self.start_turn() if force_turn else []
            self.spawn([str(self.args.binary), 'serve-relay', '--bind', '127.0.0.1', '--port',
                        str(self.args.port), '--public-origin', origin, '--allow-http-loopback', *turn], 'relay')
        state = self.root / 'connector'
        state.mkdir(mode=0o700)
        ready = self.root / 'ready.json'
        self.spawn([str(self.args.binary), 'connect-relay', '--relay-url', origin,
                    '--core-socket', self.env['HC_SOCKET'], '--state-dir', str(state),
                    '--ready-file', str(ready),
                    *([] if external else ['--allow-http-loopback']),
                    *(['--force-turn'] if force_turn else [])], 'connector')
        deadline = time.monotonic() + 45
        while not ready.is_file():
            require(all(p.poll() is None for p in self.children), 'relay or connector exited')
            require(time.monotonic() < deadline, 'connector readiness timeout')
            time.sleep(.1)
        pairing = json.loads((state / 'pairing.json').read_text())
        # A second independent connector identity deliberately shares the fixture
        # Core/layout IDs: browser credentials and private views must stay isolated.
        second_state = self.root / 'second-connector'
        second_state.mkdir(mode=0o700)
        second_ready = self.root / 'second-ready.json'
        self.spawn([str(self.args.binary), 'connect-relay', '--relay-url', origin,
                    '--core-socket', self.env['HC_SOCKET'], '--state-dir', str(second_state),
                    '--ready-file', str(second_ready),
                    *([] if external else ['--allow-http-loopback']),
                    *(['--force-turn'] if force_turn else [])], 'second-connector')
        deadline = time.monotonic() + 45
        while not second_ready.is_file():
            require(all(p.poll() is None for p in self.children), 'relay or connector exited')
            require(time.monotonic() < deadline, 'connector readiness timeout')
            time.sleep(.1)
        second_pairing = json.loads((second_state / 'pairing.json').read_text())
        private(self.workspace / 'job.json', json.dumps({'origin': origin, 'pairing': pairing, 'secondPairing': second_pairing,
                'transport': self.args.transport, 'proofDir': str(self.root)}))
        # Explicit exception confined to this disposable browser; NEVER native TLS.
        browser_flags = ['--ignore-certificate-errors'] if self.args.transport == 'tls' and not external else []
        self.gui_call('launch', 'chromium', *browser_flags, 'about:blank')
        paired = self.browser('pair')
        require((self.root / 'shell-proof').read_text() == 'relay-shell-proof\n', 'shell file proof failed')
        self.browser('uploads')
        require(not (self.root / 'unexpected-enter').exists(), 'upload submitted shell draft')
        paths = json.loads((self.workspace / 'uploads.json').read_text())
        source = (self.workspace / 'upload.png').read_bytes()
        require(len(paths) == 21 and len(set(paths)) == 21, 'upload paths not distinct')
        for name in paths:
            path = Path(name)
            require(path.is_absolute() and path.resolve().is_relative_to(self.root), 'upload escaped isolated Core')
            require(path.read_bytes() == source, 'native uploaded bytes differ')
        self.browser('reload')
        require((self.root / 'reload-proof').read_text() == 'reload-proof\n', 'reload shell proof failed')
        self.browser('resume')
        require((self.root / 'resume-proof').read_text() == 'resume-proof\n', 'resume shell proof failed')
        self.browser('connections')
        require((self.root / 'launch-proof').read_text() == 'launch-proof\n', 'launch selection shell proof failed')
        self.browser('mobile')
        require((self.root / 'mobile-proof').read_text() == 'mobile-proof\n', 'mobile shell proof failed')
        require((self.root / 'mobile-resume-proof').read_text() == 'mobile-resume-proof\n', 'mobile resume shell proof failed')
        self.browser('multi-computer')
        require((self.root / 'second-computer-proof').read_text() == 'second-computer-proof\n', 'second computer shell proof failed')
        require((self.root / 'first-computer-proof').read_text() == 'first-computer-proof\n', 'first computer resume proof failed')
        large = self.browser('large')
        require((self.root / 'large-proof').read_text() == 'large-proof\n', 'large output shell proof failed')
        require(all(p.poll() is None for p in self.children), 'child exited during acceptance')
        report = {'ok': True, 'transport': self.args.transport, 'externalRelay': bool(external), 'pairs': paired['pairs'],
                  'checks': ['shell-file', 'upload-bytes', 'no-enter', '20-sequential-uploads',
                             'reload', 'resume', 'connections-root', 'launch-resume', 'phone-input', 'phone-reconnect', 'two-remembered-computers', '400k-output'], 'receivedBytes': large['receivedBytes']}
        if self.args.artifacts:
            self.args.artifacts.mkdir(parents=True, exist_ok=True)
            private(self.args.artifacts / ('report-' + secrets.token_hex(4) + '.json'), json.dumps(report, indent=2))
        print(json.dumps(report))

    def close(self):
        errors = []
        for proc in reversed(self.children):
            try:
                if proc.poll() is None:
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(proc.pid, signal.SIGTERM)
                    try:
                        proc.wait(timeout=8)
                    except subprocess.TimeoutExpired:
                        with contextlib.suppress(ProcessLookupError):
                            os.killpg(proc.pid, signal.SIGKILL)
                        proc.wait(timeout=5)
            except Exception:
                errors.append('child')
        for label, cleanup in [
            ('Core', lambda: self.hc_call('server', 'stop') if self.core_started else None),
            ('coturn', lambda: run(['podman', 'rm', '-f', self.container]) if self.container else None),
            ('GUI', lambda: self.gui_call('release') if self.gui else None),
        ]:
            try:
                cleanup()
            except Exception:
                errors.append(label)
        try:
            shutil.rmtree(self.root)
        except OSError:
            errors.append('private files')
        if errors:
            print('Cleanup could not confirm removal: ' + ', '.join(errors), file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=lambda p: Path(p).expanduser().resolve(),
                        default=HERE.parent / 'target/release/herdr-webapp')
    parser.add_argument('--gui-cli', type=lambda p: Path(p).expanduser().resolve(), required=True)
    parser.add_argument('--transport', choices=['direct', 'udp', 'tcp', 'tls'], default='direct')
    parser.add_argument('--port', type=int, default=9001)
    parser.add_argument('--relay-origin', help='Existing canonical HTTPS relay; no local relay/coturn or browser certificate exception')
    parser.add_argument('--artifacts', type=lambda p: Path(p).expanduser().resolve())
    args = parser.parse_args()
    require(1 <= args.port <= 65535, 'invalid port')
    if args.relay_origin is not None:
        import urllib.parse
        parsed = urllib.parse.urlsplit(args.relay_origin)
        require(parsed.scheme == 'https' and parsed.hostname and not parsed.username and not parsed.password
                and not parsed.path and not parsed.query and not parsed.fragment,
                'relay origin must be bare HTTPS without credentials, path, query or fragment')
    old_umask = os.umask(0o077)
    harness = None
    def interrupted(*_):
        raise Failure('interrupted')
    signal.signal(signal.SIGINT, interrupted)
    signal.signal(signal.SIGTERM, interrupted)
    try:
        harness = Harness(args)
        harness.execute()
        return 0
    except Exception as error:
        phase = harness.phase if harness else 'setup'
        label = str(error) if isinstance(error, Failure) else 'unexpected fixture failure'
        print('FAIL: ' + phase + ': ' + label + ' (private diagnostics suppressed)', file=sys.stderr)
        if args.artifacts:
            with contextlib.suppress(Exception):
                args.artifacts.mkdir(parents=True, exist_ok=True)
                private(args.artifacts / ('report-' + secrets.token_hex(4) + '.json'),
                        json.dumps({'ok': False, 'phase': phase, 'transport': args.transport}))
        return 1
    finally:
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        if harness:
            harness.close()
        os.umask(old_umask)


if __name__ == '__main__':
    sys.exit(main())
