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
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

PROTOCOL = 1
MAX_MANIFEST = 128 * 1024
MAX_BINARY = 256 * 1024 * 1024
REVISION = re.compile(r'(?:[0-9a-f]{40}|[0-9a-f]{64})\Z')
TARGETS = ('x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'aarch64-apple-darwin')
VERSION = re.compile(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z')


class Error(Exception):
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
        if child.returncode != 0 or text.strip() != (('herdr-webapp ' if flag == '--version' else '') + expected).encode():
            raise Error('client version probe mismatch')
    finally:
        stop(child)
        child.stdout.close()


def release_identity(asset):
    return asset['version'] + ('-' + asset['revision'] if asset.get('channel') == 'main' else '')


def probe_release(path, asset):
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


class Supervisor:
    def __init__(self, args):
        self.args = args
        self.relay = origin(args.relay_url, args.allow_http_loopback)
        key = hashlib.sha256(self.relay.encode()).hexdigest()[:32]
        self.cache = private_dir(private_dir(args.cache_dir) / key)
        self.state = private_dir(private_dir(args.state_dir) / key)
        self.client = Client(self.relay, args.network_timeout)
        self.child = None
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
            self.child = subprocess.Popen([str(path), 'connect-relay', '--relay-url', self.relay,
                                           '--state-dir', str(self.state), '--ready-file', ready])
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
                        or data.get('ready') is not True or data.get('version') != release['version']
                        or (release.get('channel') == 'main' and (data.get('channel') != 'main' or data.get('revision') != release['revision']))):
                    raise Error('connector readiness did not match the launched child')
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
            self.save(asset)
            self.prune()
            next_poll = time.monotonic() + self.args.poll_interval
            while True:
                code = self.child.poll()
                if code is not None:
                    return code
                if time.monotonic() >= next_poll:
                    try:
                        update = self.client.manifest(wanted)
                        changed = release_identity(update) != release_identity(asset) if self.client.base else version(update['version']) > version(asset['version'])
                        if changed and release_identity(update) not in self.failed:
                            candidate = install(self.client, self.cache, update)  # old child stays live throughout download/probe
                            stop(self.child)
                            try:
                                self.start(candidate, update)
                            except Error:
                                self.failed.add(release_identity(update))
                                # Recheck cached bytes before every rollback execution too.
                                rollback = install(self.client, self.cache, asset)
                                self.start(rollback, asset)
                                print('Relay client update failed; restored previous client.', file=sys.stderr)
                            else:
                                self.previous, self.current = self.current, candidate
                                asset = update
                                self.save(asset)
                            self.prune()
                    except Error:
                        # Do not print URL, server content, or credentials from a remote exception.
                        print('Relay update unavailable; keeping current client.', file=sys.stderr)
                    next_poll = time.monotonic() + self.args.poll_interval
                time.sleep(0.05)
        except KeyboardInterrupt:
            return -(self.signal or signal.SIGINT)
        finally:
            # Ignore repeat shutdown signals while reaping our exact Popen child (never a saved PID).
            if self.signal:
                for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
                    signal.signal(sig, signal.SIG_IGN)
            stop(self.child)
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
    p.add_argument('--allow-http-loopback', action='store_true', help='development only')
    p.add_argument('--cache-dir', default=str(Path(os.environ.get('XDG_CACHE_HOME', str(Path.home() / '.cache'))) / 'herdr-webapp/relay'))
    p.add_argument('--state-dir', default=str(Path(os.environ.get('XDG_STATE_HOME', str(Path.home() / '.local/state'))) / 'herdr-webapp/relay'))
    p.add_argument('--poll-interval', type=bounded(0.1, 3600), default=30, help='seconds (sub-second values for tests only)')
    p.add_argument('--ready-timeout', type=bounded(0.1, 120), default=20)
    p.add_argument('--network-timeout', type=bounded(0.1, 60), default=15)
    args = p.parse_args()
    try:
        code = Supervisor(args).run()
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
