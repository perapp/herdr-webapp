#!/bin/sh
# Private GitHub release installer. Requires authenticated gh, Python 3.9+, and sh.
# gh release download --repo perapp/herdr-webapp --pattern install.sh --output - | sh
# HERDR_WEB_VERSION=x.y.z selects an exact published, non-prerelease version.
set -eu
umask 077
no_install_core=false
case "${1:-}" in
  --no-install-core) no_install_core=true; shift ;;
esac
[ "$#" -eq 0 ] || { printf '%s\n' 'Usage: install.sh [--no-install-core]' >&2; exit 1; }
repo=perapp/herdr-webapp
GH_HOST=github.com
GH_PROMPT_DISABLED=1
export GH_HOST GH_PROMPT_DISABLED
fail() { printf 'herdr-webapp: %s\n' "$*" >&2; exit 1; }
for tool in gh python3; do
  command -v "$tool" >/dev/null 2>&1 || fail "requires $tool (install it first)"
done
install_dir=${HERDR_WEB_INSTALL_DIR:-${XDG_DATA_HOME:-"$HOME/.local/share"}/herdr-webapp}
bin_dir=${HERDR_WEB_BIN_DIR:-"$HOME/.local/bin"}
# Reject ambiguous paths, including parent traversal and symlinked ancestors.
python3 - "$install_dir" "$bin_dir" <<'PY'
import pathlib, sys
if sys.version_info < (3, 9):
    raise SystemExit('herdr-webapp: Python 3.9 or newer is required')
for value in sys.argv[1:]:
    p = pathlib.Path(value)
    if not p.is_absolute() or '..' in p.parts or p == pathlib.Path('/') or str(p) != value:
        raise SystemExit('herdr-webapp: install/bin directories must be normalized absolute paths')
    if any(parent.is_symlink() for parent in (p, *p.parents)):
        raise SystemExit('herdr-webapp: install/bin directories must not use symlinked paths')
a, b = map(pathlib.Path, sys.argv[1:])
if a == b or a in b.parents or b in a.parents:
    raise SystemExit('herdr-webapp: install and bin directories must be separate')
PY
system=$(uname -s)
machine=$(uname -m)
# uname describes the kernel, not necessarily the userland. In particular a
# 32-bit Python/userland may run under an x86_64 or aarch64 Linux kernel.
bits=$(python3 -c 'import struct; print(struct.calcsize("P") * 8)')
case "$system/$machine/$bits" in
  Linux/x86_64/64) arch=x86_64 ;;
  Linux/x86_64/32|Linux/i386/32|Linux/i486/32|Linux/i586/32|Linux/i686/32) arch=i686 ;;
  Linux/aarch64/64|Linux/arm64/64) arch=aarch64 ;;
  Linux/aarch64/32|Linux/arm64/32|Linux/armv7l/32|Linux/armv8l/32) arch=armv7 ;;
  Darwin/arm64/64|Darwin/aarch64/64) target=aarch64-apple-darwin ;;
  Darwin/x86_64/64) target=x86_64-apple-darwin ;;
  *) fail "unsupported platform/architecture/userland: $system/$machine/$bits (no Windows builds)" ;;
esac
if [ "$system" = Linux ]; then
  # A failed getconf is not proof of musl. Require positive libc identification;
  # musl ldd commonly returns nonzero even when it prints its version.
  libc_version=$(getconf GNU_LIBC_VERSION 2>/dev/null || :)
  case "$libc_version" in
    'glibc '*)
      python3 - "$libc_version" <<'PYLIBC'
import re, sys
match = re.fullmatch(r'glibc ([0-9]+)\.([0-9]+)(?:\.[0-9]+)?', sys.argv[1])
if not match or tuple(map(int, match.groups())) < (2, 35):
    raise SystemExit('herdr-webapp: GNU Linux builds require glibc 2.35 or newer (Ubuntu 22.04 baseline)')
PYLIBC
      case "$arch" in
        armv7) target=armv7-unknown-linux-gnueabihf ;;
        *) target=$arch-unknown-linux-gnu ;;
      esac
      ;;
    *)
      libc_description=$(ldd --version 2>&1 || :)
      case "$libc_description" in
        *musl*)
          case "$arch" in
            x86_64|aarch64) target=$arch-unknown-linux-musl ;;
            *) fail "no published musl build for $arch; only x86_64/aarch64 musl userlands are supported" ;;
          esac
          ;;
        *) fail 'cannot identify Linux libc: supported builds require glibc 2.35+ or musl (x86_64/aarch64)' ;;
      esac
      ;;
  esac
fi
requested=${HERDR_WEB_VERSION:-}
if [ -n "$requested" ]; then
  python3 - "$requested" <<'PY'
import re, sys
if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', sys.argv[1]):
    raise SystemExit('herdr-webapp: HERDR_WEB_VERSION must be exact stable x.y.z (without v)')
PY
fi
parent=$(dirname -- "$install_dir")
mkdir -p -- "$parent"
# The backend requires an owned, non-group/world-writable parent as well.
python3 - "$parent" <<'PY'
import os, pathlib, sys
p = pathlib.Path(sys.argv[1])
s = p.stat()
if s.st_uid != os.getuid() or s.st_mode & 0o022:
    raise SystemExit('herdr-webapp: installation parent must be owned by you and not group/world writable')
PY
lock="${install_dir}.update-lock"
stage=
locked=false
swapped=false
committed=false
link_stage=
core_pid=
cleanup() {
  status=$?
  trap - 0 HUP INT TERM
  if [ -n "$core_pid" ]; then
    kill -TERM "$core_pid" 2>/dev/null || :
    wait "$core_pid" 2>/dev/null || :
  fi
  if [ "$swapped" = true ] && [ "$committed" != true ]; then
    rm -rf -- "$install_dir"
    if [ -d "$stage/backup" ]; then
      if ! mv -- "$stage/backup" "$install_dir"; then
        printf 'herdr-webapp: rollback needs manual recovery from %s/backup\n' "$stage" >&2
        stage=
        status=1
      fi
    fi
  fi
  if [ -n "$link_stage" ]; then rm -rf -- "$link_stage"; fi
  if [ -n "$stage" ]; then rm -rf -- "$stage"; fi
  if [ "$locked" = true ]; then rmdir -- "$lock" 2>/dev/null || true; fi
  exit "$status"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM
mkdir -- "$lock" 2>/dev/null || fail 'another installer/updater owns the update lock; wait for it to finish (do not delete an active lock)'
locked=true
# The updater uses this same lock and sibling PID record. Never signal a core or terminal.
python3 - "${install_dir}.pid" <<'PY'
import errno, os, pathlib, sys
p = pathlib.Path(sys.argv[1])
if p.is_symlink():
    raise SystemExit('herdr-webapp: refusing symlinked PID record')
if p.exists():
    text = p.read_text().strip()
    if not text.isascii() or not text.isdigit() or int(text) <= 0:
        raise SystemExit('herdr-webapp: invalid PID record; stop the web server and inspect it first')
    try:
        os.kill(int(text), 0)
    except ProcessLookupError:
        pass
    except PermissionError:
        raise SystemExit('herdr-webapp: recorded server may be running; use its UI updater or stop only the web server first')
    else:
        raise SystemExit('herdr-webapp: server is running; use its UI updater or stop only the web server first')
PY
# A receipt is ownership, not authorization to overwrite arbitrary user files.
python3 - "$install_dir" "$bin_dir/herdr-webapp" "$target" <<'PY'
import json, pathlib, re, sys
root, link = map(pathlib.Path, sys.argv[1:3])
if root.is_symlink():
    raise SystemExit('herdr-webapp: refusing symlinked installation')
if root.exists():
    receipt = root / 'install.json'
    try:
        if not root.is_dir() or receipt.is_symlink() or not receipt.is_file():
            raise ValueError()
        data = json.loads(receipt.read_text())
        if (set(data) != {'schemaVersion', 'repository', 'target', 'version'}
            or type(data['schemaVersion']) is not int or data['schemaVersion'] != 1
            or data['repository'] != 'perapp/herdr-webapp' or data['target'] != sys.argv[3]
            or not isinstance(data['version'], str)
            or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', data['version'])):
            raise ValueError()
    except (OSError, ValueError, TypeError):
        raise SystemExit('herdr-webapp: refusing unmanaged installation (valid install.json required)')
if link.is_symlink():
    if str(link.readlink()) != str(root / 'herdr-webapp'):
        raise SystemExit('herdr-webapp: refusing to overwrite unmanaged CLI symlink')
elif link.exists():
    raise SystemExit('herdr-webapp: refusing to overwrite unmanaged CLI file/directory')
PY
stage=$(mktemp -d "$parent/.herdr-webapp-install.XXXXXXXX")
mkdir -- "$stage/download" "$stage/payload"
# gh keeps OAuth credentials in its own store; never serialize them or signed URLs.
# Suppress remote error bodies: they can contain private URLs.
gh auth status --hostname github.com >/dev/null 2>&1 || fail 'authenticate GitHub CLI first: gh auth login --hostname github.com'
endpoint="repos/$repo/releases/latest"
if [ -n "$requested" ]; then endpoint="repos/$repo/releases/tags/v$requested"; fi
if ! gh api --hostname github.com "$endpoint" --jq 'if (.draft == false and .prerelease == false) then .tag_name else empty end' >"$stage/tag" 2>/dev/null; then
  fail 'cannot read published release metadata; check gh authentication and repository access'
fi
tag=$(tr -d '\r\n' < "$stage/tag")
version=${tag#v}
python3 - "$tag" "$requested" <<'PY'
import re, sys
if not re.fullmatch(r'v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', sys.argv[1]):
    raise SystemExit('herdr-webapp: no stable published release found')
if sys.argv[2] and sys.argv[1] != 'v' + sys.argv[2]:
    raise SystemExit('herdr-webapp: release version mismatch')
PY
asset="herdr-webapp-v$version-$target.tar.gz"
if ! gh release download "$tag" --repo "$repo" --pattern "$asset" --pattern SHA256SUMS --dir "$stage/download" >/dev/null 2>&1; then
  fail 'release download failed; check gh authentication, repository access, and available platform assets'
fi
# Verify exact bytes, then validate ALL members before extracting any. No links,
# devices, duplicate paths, absolute paths, traversal, or unrecognized payloads.
python3 - "$stage" "$asset" "$target" "$version" <<'PY'
import hashlib, json, pathlib, re, shutil, sys, tarfile
stage = pathlib.Path(sys.argv[1])
asset, target, version = sys.argv[2:]
archive = stage / 'download' / asset
try:
    lines = (stage / 'download/SHA256SUMS').read_text().splitlines()
    checksums = {}
    for line in lines:
        match = re.fullmatch(r'([a-fA-F0-9]{64}) [ *]([^/\\\s]+)', line)
        if not match or match[2] in checksums:
            raise ValueError('invalid checksum manifest')
        checksums[match[2]] = match[1].lower()
    digest = hashlib.sha256()
    with archive.open('rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            digest.update(chunk)
    if digest.hexdigest() != checksums.get(asset):
        raise ValueError('checksum mismatch')
    required = {'herdr-webapp', 'pi-extension/commands.ts', 'scripts/install-pi-extension.sh'}
    with tarfile.open(archive, 'r:gz') as tar:
        members = tar.getmembers()
        names = [m.name for m in members]
        if (len(names) != len(set(names)) or not required.issubset(names)
            or not set(names).issubset(required | {'LICENSE'})
            or any(not m.isfile() or m.size > 512 * 1024 * 1024 for m in members)):
            raise ValueError('unsafe or incomplete archive')
        for member in members:
            path = stage / 'payload' / member.name
            path.parent.mkdir(parents=True, exist_ok=True)
            with tar.extractfile(member) as src, path.open('wb') as dst:
                shutil.copyfileobj(src, dst)
            path.chmod(0o755 if member.name in {'herdr-webapp', 'scripts/install-pi-extension.sh'} else 0o644)
    receipt = {'schemaVersion': 1, 'repository': 'perapp/herdr-webapp', 'target': target, 'version': version}
    (stage / 'payload/install.json').write_text(json.dumps(receipt, separators=(',', ':')) + '\n')
except (OSError, ValueError, tarfile.TarError) as e:
    raise SystemExit('herdr-webapp: invalid release download: ' + str(e))
PY
# Run only verified bytes; this also rejects an incompatible CPU/glibc baseline.
if ! actual=$("$stage/payload/herdr-webapp" --version 2>/dev/null); then
  fail 'downloaded binary cannot run on this platform (GNU Linux requires glibc 2.35+; musl builds are static)'
fi
[ "$actual" = "herdr-webapp $version" ] || fail 'downloaded binary version does not match release'
# Dependency setup is explicit installer behavior, never executable startup behavior.
python3 - "$no_install_core" <<'PYCORE' &
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
def interrupted(number, frame):
    raise KeyboardInterrupt()
signal.signal(signal.SIGTERM, interrupted)
signal.signal(signal.SIGHUP, interrupted)
try:
    ensure_core(no_install=sys.argv[1] == 'true', start=True)
except KeyboardInterrupt:
    raise SystemExit('herdr-webapp Core installation interrupted; inspect the destination, then retry')
except (CoreBootstrapError, OSError, subprocess.TimeoutExpired) as error:
    raise SystemExit('herdr-webapp Core setup failed: ' + str(error))
PYCORE
core_pid=$!
if wait "$core_pid"; then core_pid=; else core_pid=; fail 'Core setup failed; connector installation was not committed'; fi
mkdir -p -- "$bin_dir"
link_stage=$(mktemp -d "$bin_dir/.herdr-webapp-link.XXXXXXXX")
ln -s -- "$install_dir/herdr-webapp" "$link_stage/herdr-webapp"
# Prepare rollback before either rename. The existing installation never gets
# unpacked into or modified in place. Both directory renames stay on one device.
if [ -d "$install_dir" ]; then mv -- "$install_dir" "$stage/backup"; fi
swapped=true
mv -- "$stage/payload" "$install_dir"
# Python os.replace avoids mv following a destination symlink-to-directory.
python3 - "$link_stage/herdr-webapp" "$bin_dir/herdr-webapp" <<'PY'
import os, sys
os.replace(sys.argv[1], sys.argv[2])
PY
committed=true
printf 'Installed herdr-webapp %s (%s). Run: %s/herdr-webapp\n' "$version" "$target" "$bin_dir"
printf '%s\n' 'Pi command metadata is optional: run the included scripts/install-pi-extension.sh only if wanted.'
case ":$PATH:" in *":$bin_dir:"*) ;; *) printf 'Add %s to PATH if needed.\n' "$bin_dir" ;; esac
