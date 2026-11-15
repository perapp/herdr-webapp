#!/usr/bin/env python3
"""Fast Vite development with a disposable Core, shell and trusted-user login.

No watcher/release installer and no access to the inherited Core session.
"""
import argparse
import http.client
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit

PROJECT = Path(__file__).resolve().parents[1]


def origin_value(value):
    try:
        parsed = urlsplit(value)
        if (parsed.scheme != 'http' or not parsed.hostname or parsed.username is not None
                or parsed.password is not None or parsed.path or parsed.query or parsed.fragment
                or '?' in value or '#' in value or any(c.isspace() or ord(c) < 32 for c in value)
                or '\\' in value or parsed.port == 0):
            raise ValueError()
        # Trigger validation of malformed ports, including out-of-range values.
        parsed.port
    except ValueError:
        raise argparse.ArgumentTypeError('origin must be an exact http://host[:port] authority (no path, credentials, query or fragment)') from None
    return value


def discover_origin(port):
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.connect(('192.0.2.1', 9))  # Route lookup; no packet is sent.
            host = sock.getsockname()[0]
        if host.startswith('127.') or host == '0.0.0.0':
            raise OSError()
    except OSError:
        raise RuntimeError('Cannot discover a LAN route; pass --origin http://localhost:9001 explicitly') from None
    return f'http://{host}:{port}'


def isolated_env(root, inherited):
    # Deliberate whitelist: no HC_*, HERDR_*, shell startup hooks or npm/node hooks.
    env = {key: inherited[key] for key in ('PATH', 'LANG', 'LC_ALL', 'TERM', 'COLORTERM', 'USER', 'LOGNAME') if key in inherited}
    env['SHELL'] = '/bin/bash'
    for key, name in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'), ('XDG_STATE_HOME', 'state'),
                      ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache'), ('XDG_RUNTIME_DIR', 'run')]:
        folder = root / name
        folder.mkdir(mode=0o700)
        env[key] = str(folder)
    env.update(HC_SESSION='web-dev', HC_SOCKET=str(root / 'core.sock'))
    return env


def private_write(path, value):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as output:
        output.write(value)


def validate_state_file(path):
    path = path.absolute()
    parent = path.parent.stat()
    if parent.st_uid != os.getuid() or stat.S_IMODE(parent.st_mode) & 0o077:
        raise RuntimeError('--state-file parent must be owned by you and private (0700)')
    if path.exists() or path.is_symlink():
        raise RuntimeError('--state-file must not already exist')
    return path


def check_ports(ui_port, backend_port):
    if ui_port == backend_port:
        raise RuntimeError('UI and backend ports must differ')
    sockets = []
    try:
        for host, port in [('0.0.0.0', ui_port), ('127.0.0.1', backend_port)]:
            sock = socket.socket()
            sockets.append(sock)
            sock.bind((host, port))
    except OSError:
        raise RuntimeError('A requested dev port is occupied; choose --port / --backend-port (no existing server was stopped)') from None
    finally:
        for sock in sockets:
            sock.close()


def stop_group(proc):
    # Only Popen children created with start_new_session=True are accepted here.
    for sig, grace in [(signal.SIGINT, 2), (signal.SIGTERM, 1), (signal.SIGKILL, 1)]:
        try:
            os.killpg(proc.pid, sig)
        except ProcessLookupError:
            break
        try:
            proc.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            continue
        # Reap descendants in the owned group even if its leader has exited.
        if sig != signal.SIGKILL:
            continue
        break
    proc.wait(timeout=2)


class Dev:
    def __init__(self, args, hc, binary, origin):
        self.args, self.hc, self.binary, self.origin = args, hc, binary, origin
        self.root = Path(tempfile.mkdtemp(prefix='herdr-web-dev-'))
        self.root.chmod(0o700)
        self.env = isolated_env(self.root, os.environ)
        self.children = []
        self.core_attempted = False
        self.external_state = None

    def hc_call(self, *args):
        proc = subprocess.Popen([self.hc, *args], env=self.env, cwd=self.root,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
        try:
            output, _ = proc.communicate(timeout=15)
        except BaseException:
            stop_group(proc)
            raise
        if proc.returncode:
            raise RuntimeError('Isolated hc command failed')
        return output

    def rpc(self, method, params):
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(5)
            sock.connect(self.env['HC_SOCKET'])
            with sock.makefile('rw') as stream:
                for index, (name, value) in enumerate([('hello', {'generation': 1}), (method, params)]):
                    stream.write(json.dumps({'jsonrpc': '2.0', 'id': index, 'method': name, 'params': value}) + '\n')
                    stream.flush()
                    result = json.loads(stream.readline())
                    if 'error' in result:
                        raise RuntimeError('Isolated Core RPC failed')
                return result['result']

    def spawn(self, argv, name, cwd=None, env=None, dashboard=False):
        log_path = self.root / f'{name}.log'
        fd = os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'wb') as log:
            proc = subprocess.Popen(argv, env=env or self.env, cwd=cwd or self.root,
                                    stdin=None if dashboard else subprocess.DEVNULL,
                                    stdout=None if dashboard else log, stderr=log, start_new_session=True)
        self.children.append(proc)
        return proc

    def ready(self, port, path, expected, deadline):
        while time.monotonic() < deadline:
            if any(child.poll() is not None for child in self.children):
                raise RuntimeError('A dev child exited during startup (use --keep to inspect private logs)')
            conn = http.client.HTTPConnection('127.0.0.1', port, timeout=0.5)
            try:
                conn.request('GET', path)
                response = conn.getresponse()
                response.read()
                if response.status == expected:
                    return
            except (OSError, http.client.HTTPException):
                pass
            finally:
                conn.close()
            time.sleep(0.05)
        raise RuntimeError('Dev startup timed out after 30 seconds')

    def start(self):
        self.core_attempted = True  # Stop a partially successful start as well.
        self.hc_call('server', 'start')
        terminal = json.loads(self.hc_call('run', '--bg', '--name', 'dev-shell', '--label',
                              'herdr.space=dev-space', '--label', 'herdr.tab=dev-tab',
                              '/bin/bash', '--noprofile', '--norc'))['address']
        self.rpc('doc.set', {'name': 'herdr-app', 'version': 0, 'value': {'spaces': [
            {'id': 'dev-space', 'name': 'Development', 'tabs': [{'id': 'dev-tab', 'name': 'Terminal',
             'root': {'id': 'dev-root', 'type': 'row', 'children': [{'id': 'dev-pane', 'type': 'block',
             'content': {'terminal': terminal}}]}}]}]}})
        token = secrets.token_urlsafe(32)
        token_file = self.root / 'token'
        private_write(token_file, token)
        print(f'Dev UI: {self.origin}', flush=True)
        print(f'Private sign-in link / session details: {self.args.state_file or self.root / "connection.json"}', flush=True)
        dashboard = sys.stdout.isatty() and sys.stdin.isatty()
        backend = self.spawn([str(self.binary), '--bind', '127.0.0.1', '--port', str(self.args.backend_port),
                             '--core-socket', self.env['HC_SOCKET'], '--token-file', str(token_file),
                             '--public-origin', self.origin] + ([] if dashboard else ['--no-dashboard']),
                             'backend', dashboard=dashboard)
        vite_env = self.env | {'HERDR_API_URL': f'http://127.0.0.1:{self.args.backend_port}',
                               'HERDR_DEV_PORT': str(self.args.port), 'HERDR_DEV_ORIGIN': self.origin}
        vite = self.spawn([str(PROJECT / 'frontend/node_modules/.bin/vite'), '--host', '0.0.0.0'],
                          'vite', cwd=PROJECT / 'frontend', env=vite_env)
        deadline = time.monotonic() + 30
        self.ready(self.args.backend_port, '/api/mode', 200, deadline)
        self.ready(self.args.port, '/', 200, deadline)
        self.ready(self.args.port, '/api/mode', 200, deadline)
        self.ready(self.args.port, '/api/session', 401, deadline)
        state = json.dumps({'baseOrigin': self.origin, 'privateSignInUrl': f'{self.origin}/#token={token}',
                            'tokenFile': str(token_file), 'coreSocket': self.env['HC_SOCKET'],
                            'root': str(self.root), 'childPIDs': [backend.pid, vite.pid]}, indent=2) + '\n'
        private_write(self.root / 'connection.json', state)
        if self.args.state_file:
            private_write(self.args.state_file, state)
            self.external_state = self.args.state_file
        if not dashboard:
            print('Ready. Ctrl+C stops only this disposable dev session.', flush=True)
        while True:
            if any(child.poll() is not None for child in self.children):
                raise RuntimeError('A dev child exited; stopping this disposable session')
            time.sleep(0.2)

    def close(self):
        for proc in reversed(self.children):
            stop_group(proc)
        # Never derive this target from inherited environment or arbitrary state files.
        expected = self.root / 'core.sock'
        if self.core_attempted and self.env['HC_SOCKET'] == str(expected) and expected.parent == self.root:
            try:
                self.hc_call('server', 'stop')
            except (RuntimeError, subprocess.TimeoutExpired, OSError):
                print('Warning: isolated Core stop failed.', file=sys.stderr)
        if self.external_state:
            self.external_state.unlink(missing_ok=True)
        if self.args.keep:
            print(f'Kept PRIVATE dev files: {self.root} (contains login credentials; delete when done)', file=sys.stderr)
        else:
            shutil.rmtree(self.root)


def port_value(value):
    port = int(value)
    if not 1 <= port <= 65535:
        raise argparse.ArgumentTypeError('port must be 1..65535')
    return port


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--no-build', action='store_true', help='fail rather than bootstrap a missing backend')
    parser.add_argument('--binary', type=Path, default=PROJECT / 'target/debug/herdr-webapp')
    parser.add_argument('--port', type=port_value, default=9001)
    parser.add_argument('--backend-port', type=port_value, default=9002)
    parser.add_argument('--origin', type=origin_value, help='exact public HTTP Vite origin; default: LAN route')
    parser.add_argument('--state-file', type=Path, help='also write private connection JSON in an existing 0700 directory')
    parser.add_argument('--keep', action='store_true', help='retain sensitive disposable files after exit')
    args = parser.parse_args()
    dev = None
    def interrupted(signum, frame):
        raise KeyboardInterrupt()
    previous = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        hc = shutil.which('hc')  # Resolve before replacing HOME / HC_*.
        if not hc:
            raise RuntimeError('Installed hc executable not found on PATH')
        hc = str(Path(hc).absolute())
        origin = args.origin or discover_origin(args.port)
        if (urlsplit(origin).port or 80) != args.port:
            raise RuntimeError('--origin port must match --port')
        if args.state_file:
            args.state_file = validate_state_file(args.state_file)
        check_ports(args.port, args.backend_port)
        binary = args.binary.resolve()
        if not args.no_build and not binary.exists():
            # Bootstrap only: UI HMR must never trigger Cargo or asset embedding.
            # Run make dev-build explicitly after backend source changes.
            subprocess.run(['make', 'dev-build'], cwd=PROJECT, check=True)
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise RuntimeError('Backend binary missing/not executable; run make dev-build or pass --binary')
        if not (PROJECT / 'frontend/node_modules/.bin/vite').is_file():
            raise RuntimeError('Vite missing; run make frontend-deps')
        dev = Dev(args, hc, binary, origin)
        dev.start()
    except KeyboardInterrupt:
        return 0
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError) as error:
        # No captured subprocess output or credentials in normal logs.
        print(f'dev: {error}', file=sys.stderr)
        return 1
    finally:
        # A second interrupt must not skip cleanup of owned processes.
        for sig in previous:
            signal.signal(sig, signal.SIG_IGN)
        try:
            if dev:
                dev.close()
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)


if __name__ == '__main__':
    sys.exit(main())
