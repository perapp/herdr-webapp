#!/usr/bin/env python3
"""Private, per-relay connector supervisor. Python 3.9+, standard library only."""
import argparse
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import platform
import re
import selectors
import signal
import stat
import socketserver
import threading
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

# BEGIN CORE BOOTSTRAP
import shutil
import socket
import signal
import time


class CoreBootstrapError(Exception):
    pass


def core_socket_path():
    if os.environ.get('HC_SOCKET'):
        return Path(os.environ['HC_SOCKET']), True
    config = Path(os.environ.get('XDG_CONFIG_HOME', str(Path.home() / '.config'))) / 'herdr-core/config.toml'
    # Leave custom configuration to the connector's TOML parser, including older Python.
    if config.exists() and re.search(r'^\s*socket\s*=', config.read_text(), re.MULTILINE):
        return None, True
    session = os.environ.get('HC_SESSION', 'default').strip() or 'default'
    if session in ('.', '..') or any(c in '/\\:*?\"<>|' or ord(c) < 32 for c in session):
        raise CoreBootstrapError('invalid HC_SESSION; choose a simple session name')
    runtime = os.environ.get('XDG_RUNTIME_DIR')
    if not runtime and Path('/run/user/' + str(os.getuid())).is_dir():
        runtime = '/run/user/' + str(os.getuid())
    base = Path(runtime) / 'herdr-core' if runtime else Path(os.environ.get('XDG_STATE_HOME', str(Path.home() / '.local/state'))) / 'herdr-core' / session
    return base / (session + '.sock'), False


def core_hello(path):
    try:
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(5)
            sock.connect(str(path))
            sock.sendall(b'{"jsonrpc":"2.0","id":1,"method":"hello","params":{"generation":1,"framed":true}}\n')
            data = bytearray()
            deadline = time.monotonic() + 5
            while b'\n' not in data and len(data) <= 16384:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError('Core handshake timed out')
                sock.settimeout(remaining)
                block = sock.recv(4096)
                if not block:
                    break
                data.extend(block)
            if len(data) > 16384 or b'\n' not in data:
                raise ValueError('unbounded Core reply')
            reply = json.loads(data.split(b'\n')[0])
            hello = reply.get('result', {}) if isinstance(reply, dict) else {}
            if not isinstance(hello, dict) or type(hello.get('generation')) is not int or hello.get('generation') != 1 or hello.get('framed') is not True:
                raise CoreBootstrapError('running Core is incompatible: generation 1 with framed streams is required; explicitly update Core (hc update), then retry; no live Core was changed')
            return True
    except (FileNotFoundError, ConnectionRefusedError):
        return False
    except (OSError, ValueError, TypeError):
        raise CoreBootstrapError('cannot verify configured Core socket; inspect HC_SOCKET/Core permissions and connectivity; no Core was installed or restarted') from None


def core_candidates():
    dirs = os.environ.get('PATH', '').split(os.pathsep)
    dirs += [os.environ.get('HC_INSTALL_DIR', str(Path.home() / '.local/bin')), '/opt/homebrew/bin', '/usr/local/bin', str(Path.home() / '.local/share/mise/shims'), str(Path.home() / '.nix-profile/bin'), '/run/current-system/sw/bin', str(Path.home() / '.cargo/bin'), str(Path.home() / '.local/share/herdr-core/current/Contents/MacOS'), str(Path.home() / '.local/share/herdr-core/Herdr Core.app/Contents/MacOS')]
    for directory in dict.fromkeys(dirs):
        if directory:
            for name in ('herdr-core', 'hc'):
                candidate = Path(directory) / name
                if candidate.is_file():
                    yield candidate


def core_update_hint(path):
    value = str(path.resolve())
    if '/Cellar/' in value or '/homebrew/' in value:
        return 'brew upgrade herdr-core'
    if '/nix/' in value or '/.nix-profile/' in str(path):
        return 'update herdr-core in your Nix configuration'
    if '/mise/' in value:
        return 'mise upgrade herdr-core'
    return 'hc update (explicitly, after reviewing Core compatibility)'


def core_download(url, destination):
    result = subprocess.run(['curl', '--fail', '--silent', '--show-error', '--location', '--proto', '=https', '--proto-redir', '=https', '--connect-timeout', '10', '--max-time', '120', '--max-filesize', '1048576', '--output', str(destination), url], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if result.returncode:
        raise CoreBootstrapError('Core download failed; check HTTPS connectivity and curl, then rerun; nothing was started')


def core_run_installer(installer, env):
    child = subprocess.Popen(['sh', str(installer)], env=env, stdin=subprocess.DEVNULL, start_new_session=True)
    try:
        return child.wait(timeout=300)
    except BaseException:
        # Reap only this fresh installer group, never an existing Core/session.
        try:
            os.killpg(child.pid, signal.SIGTERM)
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
        except ProcessLookupError:
            child.wait()
        raise


def ensure_core(no_install=False, start=True):
    if no_install:
        return
    path, explicit = core_socket_path()
    try:
        if path is not None and core_hello(path):
            return  # A live compatible core needs no local executable at all.
    except CoreBootstrapError as error:
        if 'incompatible' in str(error):
            candidate = next(core_candidates(), None)
            if candidate is not None:
                raise CoreBootstrapError(str(error) + '; installation update instruction: ' + core_update_hint(candidate)) from None
        raise
    if explicit or no_install:
        return  # Explicit endpoints/configuration are never replaced by local setup.
    found = None
    for candidate in core_candidates():
        try:
            result = subprocess.run([str(candidate), '--version'], stdin=subprocess.DEVNULL, capture_output=True, timeout=5)
        except (OSError, subprocess.TimeoutExpired):
            if candidate.name == 'herdr-core':
                raise CoreBootstrapError('existing herdr-core cannot run; repair its installation or use --no-install-core; it was not replaced') from None
            continue
        if result.returncode == 0 and re.fullmatch(rb'hc [0-9]+\.[0-9]+\.[0-9]+[^\r\n]*\s*', result.stdout):
            found = candidate
            break
        if candidate.name == 'herdr-core':
            raise CoreBootstrapError('existing herdr-core is unusable; repair it with ' + core_update_hint(candidate))
    if found is None:
        if not shutil.which('curl') or not shutil.which('tar'):
            raise CoreBootstrapError('missing Core: install curl and tar, then rerun; or install Core yourself and use --no-install-core')
        install_dir = Path(os.environ.get('HC_INSTALL_DIR', str(Path.home() / '.local/bin'))).expanduser()
        if not install_dir.is_absolute():
            raise CoreBootstrapError('HC_INSTALL_DIR must be absolute')
        install_dir.mkdir(parents=True, exist_ok=True)
        # The official installer can update an existing binary: never call it for one.
        lock = install_dir / '.herdr-web-core-install.lock'
        try:
            lock.mkdir(mode=0o700)
        except FileExistsError:
            raise CoreBootstrapError('another Core installation may be running; wait and retry. After interruption inspect/remove HC_INSTALL_DIR/.herdr-web-core-install.lock only when no installer is active') from None
        try:
            if os.path.lexists(install_dir / 'herdr-core'):
                raise CoreBootstrapError('Core appeared during setup; rerun to reuse it (nothing was overwritten)')
            releases = os.environ.get('HERDR_RELEASES_URL', 'https://releases.herdr.dev').rstrip('/')
            parsed = urllib.parse.urlsplit(releases)
            if parsed.scheme != 'https' or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
                raise CoreBootstrapError('HERDR_RELEASES_URL must be an HTTPS release root without credentials/query/fragment')
            with tempfile.TemporaryDirectory(prefix='herdr-core-bootstrap-') as temp:
                manifest = Path(temp) / 'manifest.json'
                core_download(releases + '/core/latest.json', manifest)
                try:
                    metadata = json.loads(manifest.read_bytes())
                    release = metadata['version']
                    if type(metadata.get('generation')) is not int or metadata['generation'] != 1 or not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', release):
                        raise ValueError()
                except (ValueError, KeyError, TypeError):
                    raise CoreBootstrapError('latest Core release is not compatible with required generation 1; install a compatible Core explicitly; main uses stable Core too') from None
                installer = Path(temp) / 'install.sh'
                core_download(releases + '/core/install.sh', installer)
                print('Herdr Web needs Herdr Core; installing compatible stable Core ' + release + ' using its official installer.', file=sys.stderr)
                env = dict(os.environ, HC_VERSION=release, HC_INSTALL_DIR=str(install_dir))
                env.pop('HC_BUNDLE_ONLY', None)
                if core_run_installer(installer, env):
                    raise CoreBootstrapError('official Core installation failed/interrupted; check disk space, permissions and network; rerun to verify/repair before launching')
            found = install_dir / 'herdr-core'
        finally:
            lock.rmdir()
        try:
            result = subprocess.run([str(found), '--version'], capture_output=True, timeout=5)
        except (OSError, subprocess.TimeoutExpired):
            raise CoreBootstrapError('installer completed but Core cannot run; inspect HC_INSTALL_DIR and platform requirements') from None
        if result.returncode or not result.stdout.startswith(b'hc '):
            raise CoreBootstrapError('installer completed but Core verification failed; no connector was started')
    os.environ['PATH'] = str(found.parent) + os.pathsep + os.environ.get('PATH', '')
    if start:
        # server start is the supported idempotent lifecycle; never restart/update.
        result = subprocess.run([str(found), 'server', 'start', '--json'], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
        if result.returncode or not core_hello(path):
            raise CoreBootstrapError('Core could not start with required generation 1/framed streams; use ' + core_update_hint(found) + '; inspect hc server status --json. No Core update/restart was requested')
# END CORE BOOTSTRAP

PROTOCOL = 1
MAX_MANIFEST = 128 * 1024
MAX_BINARY = 256 * 1024 * 1024
REVISION = re.compile(r'(?:[0-9a-f]{40}|[0-9a-f]{64})\Z')
TARGETS = ('x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'aarch64-apple-darwin')
VERSION = re.compile(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z')


class Error(Exception):
    pass


class UnsupportedOption(Error):
    pass


class Incompatible(Error):
    pass


def version(value):
    if not isinstance(value, str) or len(value) > 64 or not VERSION.fullmatch(value):
        raise Error('invalid release version (expected stable x.y.z)')
    return tuple(map(int, value.split('.')))


def origin(value, allow_http=False):
    try:
        u = urllib.parse.urlsplit(value)
        host = u.hostname
        port = u.port
        if not host or u.username is not None or u.password is not None:
            raise ValueError()
        if u.path not in ('', '/', '/main', '/main/') or '?' in value or '#' in value:
            raise ValueError()
        if any(c.isspace() for c in value) or not re.fullmatch(r'[A-Za-z0-9.:[\]-]+', u.netloc):
            raise ValueError()
        loopback = host == 'localhost'
        try:
            loopback = ipaddress.ip_address(host).is_loopback
        except ValueError:
            pass
        if u.scheme != 'https' and not (allow_http and u.scheme == 'http' and loopback):
            raise ValueError()
        authority = '[' + host.lower() + ']' if ':' in host else host.lower()
        if port is not None and port != (443 if u.scheme == 'https' else 80):
            authority += ':' + str(port)
        return u.scheme + '://' + authority + ('/main' if u.path.startswith('/main') else '')
    except ValueError:
        raise Error('relay URL must be an HTTPS origin or /main endpoint (HTTP requires explicit loopback development mode)') from None


def target():
    machine = platform.machine().lower()
    arch = {'amd64': 'x86_64', 'x86_64': 'x86_64', 'arm64': 'aarch64', 'aarch64': 'aarch64'}.get(machine)
    if platform.system() == 'Linux' and arch:
        if platform.libc_ver()[0] != 'glibc':
            raise Error('only GNU libc Linux is supported')
        return arch + '-unknown-linux-gnu'
    if platform.system() == 'Darwin' and arch == 'aarch64':
        return 'aarch64-apple-darwin'
    raise Error('unsupported platform (GNU Linux x64/arm64 or Apple Silicon required)')


def private_dir(path):
    path = Path(os.path.abspath(os.path.expanduser(str(path))))
    # Refuse symlink components, including in caller-supplied cache/state roots.
    for part in [*reversed(path.parents), path]:
        try:
            st = part.lstat()
        except FileNotFoundError:
            part.mkdir(mode=0o700)
            st = part.lstat()
        if not stat.S_ISDIR(st.st_mode) or stat.S_ISLNK(st.st_mode):
            raise Error('unsafe directory component')
    if st.st_uid != os.getuid() or stat.S_IMODE(st.st_mode) != 0o700:
        raise Error('cache/state directory must be owned by you and mode 0700')
    return path


def safe_open(path, flags=os.O_RDONLY, mode=0o600):
    fd = os.open(path, flags | os.O_NOFOLLOW | os.O_NONBLOCK, mode)
    st = os.fstat(fd)
    if not stat.S_ISREG(st.st_mode) or st.st_uid != os.getuid() or st.st_nlink != 1 or st.st_mode & 0o077:
        os.close(fd)
        raise Error('unsafe private file')
    return fd


def read_json(path, limit=MAX_MANIFEST):
    with os.fdopen(safe_open(path), 'rb') as f:
        if stat.S_IMODE(os.fstat(f.fileno()).st_mode) != 0o600:
            raise Error('private JSON must have mode 0600')
        data = f.read(limit + 1)
    if len(data) > limit:
        raise Error('JSON is too large')
    try:
        return json.loads(data)
    except (ValueError, UnicodeError):
        raise Error('invalid JSON') from None


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise Error('redirects are not allowed')


class Client:
    def __init__(self, relay, timeout=15):
        self.relay = relay
        endpoint = urllib.parse.urlsplit(relay)
        self.authority = urllib.parse.urlunsplit((endpoint.scheme, endpoint.netloc, '', '', ''))
        self.base = '/main' if endpoint.path == '/main' else ''
        self.timeout = timeout
        # Ignore environment proxy configuration; never forward trusted-user traffic to a proxy.
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def stream(self, path, limit, output):
        if not isinstance(path, str) or not path.startswith('/') or path.startswith('//') or '\\' in path or '#' in path:
            raise Error('artifact URL must be an absolute same-origin path')
        parsed = urllib.parse.urlsplit(path)
        if parsed.scheme or parsed.netloc or parsed.query or any(c.isspace() for c in path):
            raise Error('invalid artifact URL')
        start = time.monotonic()
        size = 0
        digest = hashlib.sha256()
        try:
            with self.opener.open(self.authority + path, timeout=self.timeout) as response:
                if response.status != 200:
                    raise Error('unexpected HTTP status')
                while True:
                    # read1 performs at most one underlying socket read: a peer cannot
                    # indefinitely trickle bytes into a filling read(n) past our deadline.
                    data = response.read1(min(65536, limit + 1 - size))
                    if not data:
                        break
                    size += len(data)
                    if size > limit or time.monotonic() - start > self.timeout * 4:
                        raise Error('download exceeds size/time bound')
                    output.write(data)
                    digest.update(data)
        except (OSError, urllib.error.URLError, ValueError):
            raise Error('relay download failed') from None
        return size, digest.hexdigest()

    def manifest(self, wanted):
        import io
        output = io.BytesIO()
        self.stream(self.base + '/client/manifest.json', MAX_MANIFEST, output)
        try:
            data = json.loads(output.getvalue())
            if type(data['schemaVersion']) is not int or data['schemaVersion'] != (2 if self.base else 1):
                raise Incompatible('unsupported manifest schema')
            if type(data['protocolVersion']) is not int or data['protocolVersion'] != PROTOCOL:
                raise Incompatible('relay protocol is not compatible with this launcher')
            version(data['version'])
            if self.base and (data.get('channel') != 'main' or not isinstance(data.get('revision'), str) or not REVISION.fullmatch(data['revision'])):
                raise Incompatible('invalid main channel release identity')
            assets = data['assets']
            if not isinstance(assets, list) or len(assets) > 32:
                raise Error('invalid assets')
            if self.base:
                seen = set()
                for item in assets:
                    if not isinstance(item, dict) or item.get('target') not in TARGETS or item['target'] in seen:
                        raise Error('invalid main platform assets')
                    seen.add(item['target'])
                    expected_path = '/main/client/' + data['revision'] + '/' + item['target'] + '/herdr-webapp'
                    if item.get('url') != expected_path or not isinstance(item.get('sha256'), str) or not re.fullmatch('[0-9a-f]{64}', item['sha256']) or type(item.get('size')) is not int or not 0 < item['size'] <= MAX_BINARY:
                        raise Error('invalid main artifact')
                if wanted not in seen:
                    raise Incompatible('main channel does not support this platform; available targets: ' + (', '.join(sorted(seen)) or '(none)'))
            matches = [a for a in assets if isinstance(a, dict) and a.get('target') == wanted]
            if len(matches) != 1:
                raise Error('no unique artifact for this platform')
            a = matches[0]
            if not isinstance(a['sha256'], str) or not re.fullmatch('[0-9a-f]{64}', a['sha256']):
                raise Error('invalid artifact checksum')
            if type(a['size']) is not int or not 0 < a['size'] <= MAX_BINARY:
                raise Error('invalid artifact size')
            # Validate URL even when the artifact is already cached.
            url = a['url']
            if not isinstance(url, str) or not re.fullmatch(self.base + r'/client/[A-Za-z0-9._/-]+', url) or '..' in url.split('/'):
                raise Error('invalid same-origin artifact path')
            return dict(a, version=data['version'], **({'channel': 'main', 'revision': data['revision']} if self.base else {}))
        except (KeyError, TypeError, ValueError, UnicodeError):
            raise Error('malformed relay manifest') from None


def digest_file(path):
    digest = hashlib.sha256()
    size = 0
    with os.fdopen(safe_open(path), 'rb') as f:
        for block in iter(lambda: f.read(65536), b''):
            size += len(block)
            if size > MAX_BINARY:
                raise Error('cached executable is too large')
            digest.update(block)
    return size, digest.hexdigest()


def probe(path, expected, flag='--version'):
    child = subprocess.Popen([str(path), flag], stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    try:
        text = bytearray()
        until = time.monotonic() + 5
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            while True:
                remaining = until - time.monotonic()
                if remaining <= 0 or not selector.select(remaining):
                    raise Error('client version probe timed out')
                block = os.read(child.stdout.fileno(), 257 - len(text))
                if not block:
                    break
                text.extend(block)
                if len(text) > 256:
                    raise Error('client version probe output is too large')
        try:
            child.wait(timeout=max(0.01, until - time.monotonic()))
        except subprocess.TimeoutExpired:
            raise Error('client version probe timed out') from None
        if child.returncode == 2 and flag == '--package-version':
            raise UnsupportedOption('legacy client has no package-version option')
        if child.returncode != 0 or text.strip() != (('herdr-webapp ' if flag in ('--version', '--package-version') else '') + expected).encode():
            raise Error('client version probe mismatch')
    finally:
        stop(child)
        child.stdout.close()


def release_identity(asset):
    return asset['version'] + ('-' + asset['revision'] if asset.get('channel') == 'main' else '')


def probe_release(path, asset):
    try:
        probe(path, asset['version'], '--package-version' if asset.get('channel') == 'main' else '--version')
    except UnsupportedOption:
        # Only clap's unsupported-option exit permits the legacy numeric probe.
        # Successful but mismatched new output must never downgrade validation.
        probe(path, asset['version'])
    if asset.get('channel') == 'main':
        probe(path, asset['revision'], '--build-revision')


def install(client, cache, asset):
    path = cache / ('client-' + release_identity(asset))
    expected = (asset['size'], asset['sha256'])
    if path.exists() or path.is_symlink():
        if digest_file(path) != expected:
            raise Error('cached executable checksum mismatch')
        probe_release(path, asset)
        return path
    fd, tmp = tempfile.mkstemp(prefix='.download-', dir=cache)
    try:
        with os.fdopen(fd, 'wb') as output:
            if client.stream(asset['url'], asset['size'], output) != expected:
                raise Error('artifact checksum/size mismatch')
            output.flush()
            os.fsync(output.fileno())
            os.fchmod(output.fileno(), 0o700)
        probe_release(Path(tmp), asset)
        os.replace(tmp, path)
        return path
    finally:
        if os.path.lexists(tmp):
            os.unlink(tmp)


def stop(child):
    if child is not None and child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()


class UpdateControl:
    """Fixed same-user API. Network/download work never blocks status requests."""
    def __init__(self, supervisor, asset):
        self.owner = supervisor
        self.asset = asset
        self.candidate = None
        self.pending = None
        self.busy = False
        self.lock = threading.Lock()
        self.data = dict(schemaVersion=1, status='idle', currentVersion=asset['version'], availableVersion=None,
                         channel='main' if supervisor.client.base else 'stable',
                         message=None)
        # Short, private path avoids sockaddr_un limits with long XDG roots.
        if asset.get('revision'):
            self.data['currentRevision'] = asset['revision']
        self.temp = tempfile.TemporaryDirectory(prefix='herdr-update-')
        os.chmod(self.temp.name, 0o700)
        self.path = str(Path(self.temp.name) / 'control.sock')
        control = self

        class Handler(socketserver.StreamRequestHandler):
            def handle(self):
                self.connection.settimeout(2)
                try:
                    line = self.rfile.readline(16385)
                    if len(line) > 16384 or not line.endswith(b'\n'):
                        return
                    reply = control.request(json.loads(line))
                    self.wfile.write(json.dumps(reply, separators=(',', ':')).encode() + b'\n')
                except (OSError, ValueError, TypeError):
                    return

        class Server(socketserver.ThreadingUnixStreamServer):
            daemon_threads = True
        self.server = Server(self.path, Handler)
        os.chmod(self.path, 0o600)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def snapshot(self):
        return dict(self.data)

    def request(self, request):
        with self.lock:
            if (not isinstance(request, dict) or request.get('schemaVersion') != 1
                    or type(request.get('schemaVersion')) is not int
                    or request.get('action') not in ('status', 'check', 'update')
                    or set(request) - {'schemaVersion', 'action', 'expectedVersion', 'expectedRevision'}):
                return dict(self.data, status='failed', message='Invalid launcher control request.')
            action = request['action']
            if action == 'status':
                return self.snapshot()
            if self.busy:
                return self.snapshot()
            if action == 'update':
                if not self.candidate or request.get('expectedVersion') != self.candidate['version'] or (self.owner.client.base and request.get('expectedRevision') != self.candidate['revision']):
                    return dict(self.data, status='failed', message='Stale update selection; check for updates again.')
                self.busy = True
                self.data.update(status='updating', message=None)
                candidate = dict(self.candidate)
                threading.Thread(target=self.download, args=(candidate,), daemon=True).start()
            else:
                self.busy = True
                self.data.update(status='checking', message=None)
                threading.Thread(target=self.check, daemon=True).start()
            return self.snapshot()

    def check(self):
        try:
            candidate = self.owner.client.manifest(target())
            changed = release_identity(candidate) != release_identity(self.asset) if self.owner.client.base else version(candidate['version']) > version(self.asset['version'])
            with self.lock:
                self.candidate = candidate if changed else None
                self.data.update(status='available' if changed else 'current', availableVersion=candidate['version'] if changed else None,
                                 availableRevision=candidate.get('revision') if changed else None)
        except (Error, OSError):
            with self.lock:
                self.candidate = None
                self.data.update(status='failed', availableVersion=None, availableRevision=None, message='Update check failed; check relay connectivity and retry.')
        finally:
            with self.lock:
                self.busy = False

    def download(self, candidate):
        try:
            # Never refetch/select a different candidate after the user's CAS.
            path = install(self.owner.client, self.owner.cache, candidate)
            with self.lock:
                self.pending = (path, candidate)
        except (Error, OSError):
            self.finish(message='Download or verification failed; current connector is still running. Check and retry.')

    def finish(self, asset=None, message=None):
        with self.lock:
            if asset:
                self.asset = asset
                if asset.get('revision'):
                    self.data['currentRevision'] = asset['revision']
                self.candidate = None
            self.pending = None
            self.candidate = None
            self.busy = False
            self.data.update(status='failed' if message else 'current', message=message, currentVersion=getattr(self.owner, 'child_version', self.asset['version']),
                             availableVersion=None, availableRevision=None)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.temp.cleanup()


class Supervisor:
    def __init__(self, args):
        self.args = args
        self.relay = origin(args.relay_url, args.allow_http_loopback)
        key = hashlib.sha256(self.relay.encode()).hexdigest()[:32]
        self.cache = private_dir(private_dir(args.cache_dir) / key)
        self.state = private_dir(private_dir(args.state_dir) / key)
        self.client = Client(self.relay, args.network_timeout)
        self.child = None
        self.control = None
        self.signal = None
        self.current = None
        self.previous = None
        self.failed = set()

    def interrupted(self, number, frame):
        self.signal = number
        # Interrupt a blocked download/readiness wait, then clean up in run's finally.
        raise KeyboardInterrupt()

    def start(self, path, release):
        if isinstance(release, str):  # Stable API compatibility for existing callers.
            release = {'version': release}
        probe_release(path, release)
        fd, ready = tempfile.mkstemp(prefix='.ready-', dir=self.state)
        os.close(fd)
        os.unlink(ready)
        try:
            env = dict(os.environ)
            if self.control:
                env['HERDR_CONNECTOR_CONTROL_SOCKET'] = self.control.path
            else:
                env.pop('HERDR_CONNECTOR_CONTROL_SOCKET', None)
            self.child = subprocess.Popen([str(path), 'connect-relay', '--relay-url', self.relay,
                                           '--state-dir', str(self.state), '--ready-file', ready], env=env)
            until = time.monotonic() + self.args.ready_timeout
            while time.monotonic() < until:
                if self.child.poll() is not None:
                    raise Error('connector exited before readiness')
                try:
                    data = read_json(ready, 4096)
                except FileNotFoundError:
                    time.sleep(0.05)
                    continue
                except OSError:
                    raise Error('unsafe connector readiness file') from None
                if (not isinstance(data, dict) or type(data.get('schemaVersion')) is not int or data.get('schemaVersion') != 1
                        or type(data.get('pid')) is not int or data.get('pid') != self.child.pid
                        or type(data.get('protocolVersion')) is not int or data.get('protocolVersion') != PROTOCOL
                        or data.get('ready') is not True or data.get('packageVersion', data.get('version')) != release['version']
                        or (release.get('channel') == 'main' and (data.get('channel') != 'main' or data.get('revision') != release['revision']))):
                    raise Error('connector readiness did not match the launched child')
                human = data.get('version')
                self.child_version = human if isinstance(human, str) and 0 < len(human) <= 128 and not any(ord(c) < 32 for c in human) else release['version']
                if self.control:
                    self.control.data['currentVersion'] = self.child_version
                return
            raise Error('connector readiness timed out')
        except BaseException:
            stop(self.child)
            raise
        finally:
            if os.path.lexists(ready):
                os.unlink(ready)

    def save(self, asset):
        fd, tmp = tempfile.mkstemp(prefix='.current-', dir=self.cache)
        try:
            with os.fdopen(fd, 'w') as f:
                json.dump(asset, f)
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, self.cache / 'current.json')
        finally:
            if os.path.lexists(tmp):
                os.unlink(tmp)

    def prune(self):
        keep = {p.name for p in (self.current, self.previous) if p is not None}
        for p in self.cache.iterdir():
            if p.name.startswith('client-') and p.name not in keep:
                # Do not follow or chmod suspicious entries.
                fd = safe_open(p)
                os.close(fd)
                p.unlink()

    def run(self):
        lock = safe_open(self.state / 'supervisor.lock', os.O_RDWR | os.O_CREAT)
        try:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise Error('a connector supervisor for this relay is already running') from None
            for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
                signal.signal(sig, self.interrupted)
            # Recover bounded disk use after an uncatchable crash during a prior download/save.
            for p in self.cache.iterdir():
                if p.name.startswith(('.download-', '.current-')):
                    fd = safe_open(p)
                    os.close(fd)
                    p.unlink()
            saved = None
            try:
                saved = read_json(self.cache / 'current.json')
                version(saved['version'])
                if self.client.base and (saved.get('channel') != 'main' or not isinstance(saved.get('revision'), str) or not REVISION.fullmatch(saved['revision'])):
                    raise Error('invalid saved main release')
                if (not isinstance(saved['sha256'], str) or not re.fullmatch('[0-9a-f]{64}', saved['sha256'])
                        or type(saved['size']) is not int or not 0 < saved['size'] <= MAX_BINARY):
                    raise Error('invalid saved release')
            except FileNotFoundError:
                pass
            except (KeyError, TypeError):
                raise Error('invalid saved release') from None
            wanted = target()
            # A cached healthy release can start when the relay is briefly unavailable.
            try:
                asset = self.client.manifest(wanted)
            except Incompatible:
                raise
            except Error:
                if saved is None:
                    raise
                asset = saved
            if saved and not self.client.base and version(asset['version']) < version(saved['version']):
                asset = saved
            ensure_core(getattr(self.args, "no_install_core", False))
            self.control = UpdateControl(self, asset)
            try:
                candidate = install(self.client, self.cache, asset)
                self.start(candidate, asset)
            except Error:
                if not saved or release_identity(saved) == release_identity(asset):
                    raise
                self.failed.add(release_identity(asset))
                asset = saved
                candidate = install(self.client, self.cache, saved)
                self.start(candidate, saved)
            self.current = candidate
            self.control.asset = asset
            if asset.get('revision'):
                self.control.data['currentRevision'] = asset['revision']
            self.save(asset)
            self.prune()
            next_poll = time.monotonic() + self.args.poll_interval
            while True:
                code = self.child.poll()
                if code is not None:
                    return code
                if time.monotonic() >= next_poll:
                    self.control.request({'schemaVersion': 1, 'action': 'check'})
                    next_poll = time.monotonic() + self.args.poll_interval
                if getattr(self.args, 'auto_update', False):
                    with self.control.lock:
                        update = dict(self.control.candidate) if self.control.candidate and not self.control.busy else None
                    if update and release_identity(update) not in self.failed:
                        self.control.request(dict(schemaVersion=1, action='update', expectedVersion=update['version'], expectedRevision=update.get('revision')))
                with self.control.lock:
                    pending = self.control.pending
                    self.control.pending = None
                if pending:
                    candidate, update = pending
                    stop(self.child)
                    try:
                        self.start(candidate, update)
                    except Error:
                        self.failed.add(release_identity(update))
                        rollback = install(self.client, self.cache, asset)
                        self.start(rollback, asset)
                        self.control.finish(message='Connector update failed; restored previous verified connector. Check and retry.')
                    else:
                        self.previous, self.current = self.current, candidate
                        asset = update
                        self.save(asset)
                        self.control.finish(asset)
                    self.prune()
                time.sleep(0.05)
        except KeyboardInterrupt:
            return -(self.signal or signal.SIGINT)
        finally:
            # Ignore repeat shutdown signals while reaping our exact Popen child (never a saved PID).
            if self.signal:
                for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
                    signal.signal(sig, signal.SIG_IGN)
            stop(self.child)
            if self.control:
                self.control.close()
            os.close(lock)


def bounded(low, high):
    def parse(value):
        number = float(value)
        if not low <= number <= high:
            raise argparse.ArgumentTypeError('value outside supported bounds')
        return number
    return parse


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--relay-url', required=True)
    p.add_argument('--no-install-core', action='store_true', help='never install/start local Core; use configured endpoint')
    p.add_argument('--auto-update', action='store_true', help='opt in to automatic connector updates (never Core)')
    p.add_argument('--core-socket', help='explicit Core socket; skips local dependency setup')
    p.add_argument('--session', help='Core session name (HC_SESSION)')
    p.add_argument('--allow-http-loopback', action='store_true', help='development only')
    p.add_argument('--cache-dir', default=str(Path(os.environ.get('XDG_CACHE_HOME', str(Path.home() / '.cache'))) / 'herdr-webapp/relay'))
    p.add_argument('--state-dir', default=str(Path(os.environ.get('XDG_STATE_HOME', str(Path.home() / '.local/state'))) / 'herdr-webapp/relay'))
    p.add_argument('--poll-interval', type=bounded(0.1, 3600), default=30, help='seconds (sub-second values for tests only)')
    p.add_argument('--ready-timeout', type=bounded(0.1, 120), default=20)
    p.add_argument('--network-timeout', type=bounded(0.1, 60), default=15)
    args = p.parse_args()
    if args.core_socket:
        os.environ['HC_SOCKET'] = args.core_socket
    if args.session:
        os.environ['HC_SESSION'] = args.session
    try:
        code = Supervisor(args).run()
    except (CoreBootstrapError, subprocess.TimeoutExpired) as error:
        print('Core setup failed: ' + str(error), file=sys.stderr)
        return 1
    except Incompatible as error:
        print('Relay launcher incompatible: ' + str(error), file=sys.stderr)
        return 1
    except (Error, OSError, ValueError):
        print('Relay launcher failed. Check relay compatibility, connectivity and private cache/state permissions.', file=sys.stderr)
        return 1
    if code < 0:
        sig = -code
        signal.signal(sig, signal.SIG_DFL)
        os.kill(os.getpid(), sig)
        return 128 + sig
    return code


if __name__ == '__main__':
    sys.exit(main())
