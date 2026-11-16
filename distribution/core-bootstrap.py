#!/usr/bin/env python3
"""Source for the embedded dependency bootstrap (synced copies tested offline)."""
import os, sys, json, re, subprocess, tempfile, urllib.parse
from pathlib import Path
# BEGIN CORE BOOTSTRAP
import shutil
import socket
import signal
import struct
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
        # Official Core assets cover only 64-bit Linux and both 64-bit Macs.
        # Never let its uname-based installer pick a 64-bit asset for a 32-bit
        # userland, even when the kernel advertises a 64-bit architecture.
        system, _, _, _, machine = os.uname()
        if (struct.calcsize('P') != 8 or system not in ('Linux', 'Darwin')
                or machine not in ('x86_64', 'aarch64', 'arm64')):
            raise CoreBootstrapError('no official Core asset for this architecture/userland; install a compatible Core yourself (for 32-bit Linux, build Core from source), or use --no-install-core; no Core was downloaded or changed')
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
