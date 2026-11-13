#!/bin/sh
# Package an already-built, embedded-frontend release. Does not compile or publish.
# Usage: scripts/package-release.sh TARGET [BINARY [OUTPUT_DIRECTORY]]
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
python3 - "$@" <<'PY'
import gzip
import io
import os
import pathlib
import re
import subprocess
import sys
import tarfile
import tempfile

supported = {'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'aarch64-apple-darwin'}
if not 2 <= len(sys.argv) <= 4 or sys.argv[1] not in supported:
    raise SystemExit('usage: package-release.sh TARGET [BINARY [OUTPUT_DIRECTORY]] (Linux GNU x86_64/aarch64 or macOS arm64 only)')
target = sys.argv[1]
manifest = pathlib.Path('Cargo.toml').read_text()
package = re.search(r'(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)', manifest)
match = re.search(r'^version\s*=\s*"([^"]+)"\s*$', package[1], re.M) if package else None
if not match:
    raise SystemExit('cannot read package.version from Cargo.toml')
version = match[1]
if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version):
    raise SystemExit('release version must be stable x.y.z')
for key in ('HERDR_WEB_VERSION', 'RELEASE_VERSION'):
    if os.environ.get(key) and os.environ[key] != version:
        raise SystemExit(f'{key} does not match Cargo.toml')
tag = os.environ.get('RELEASE_TAG')
if os.environ.get('GITHUB_REF_TYPE') == 'tag':
    ref_tag = os.environ.get('GITHUB_REF_NAME', '')
    if tag and tag != ref_tag:
        raise SystemExit('RELEASE_TAG does not match GitHub tag')
    tag = ref_tag
if tag is not None and tag != 'v' + version:
    raise SystemExit('release tag does not match Cargo.toml')
binary = pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else f'target/{target}/release/herdr-webapp')
if not binary.is_file() or binary.is_symlink() or not os.access(binary, os.X_OK):
    raise SystemExit('release binary must be a regular executable file')
host = subprocess.check_output(['rustc', '-vV'], text=True)
host_target = next(line[6:] for line in host.splitlines() if line.startswith('host: '))
if target == host_target:
    got = subprocess.check_output([str(binary.resolve()), '--version'], text=True, timeout=15).strip()
    if got != f'herdr-webapp {version}':
        raise SystemExit('binary --version does not match Cargo.toml')
else:
    print(f'package: cross target {target}; binary execution checked at install time')
files = [('herdr-webapp', binary, 0o755),
         ('pi-extension/commands.ts', pathlib.Path('pi-extension/commands.ts'), 0o644),
         ('scripts/install-pi-extension.sh', pathlib.Path('scripts/install-pi-extension.sh'), 0o755)]
if pathlib.Path('LICENSE').is_file():
    files.append(('LICENSE', pathlib.Path('LICENSE'), 0o644))
out = pathlib.Path(sys.argv[3] if len(sys.argv) > 3 else 'dist')
out.mkdir(parents=True, exist_ok=True)
archive = out / f'herdr-webapp-v{version}-{target}.tar.gz'
# Fixed metadata and gzip header: identical inputs yield identical bytes everywhere.
fd, temporary = tempfile.mkstemp(prefix='.package-', dir=out)
try:
    with os.fdopen(fd, 'wb') as raw:
        with gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w', format=tarfile.USTAR_FORMAT) as tar:
                for name, source, mode in files:
                    if source.is_symlink() or not source.is_file():
                        raise SystemExit(f'not a regular release file: {source}')
                    data = source.read_bytes()
                    if name == 'LICENSE':
                        notice = pathlib.Path('vendor/webrtc-ice/LICENSE-MIT')
                        if notice.is_file():
                            data += b'\n\nLocally modified dependency: webrtc-ice 0.17.3\n' + notice.read_bytes()
                    entry = tarfile.TarInfo(name)
                    entry.size, entry.mode, entry.mtime = len(data), mode, 0
                    entry.uid = entry.gid = 0
                    entry.uname = entry.gname = ''
                    tar.addfile(entry, io.BytesIO(data))
    os.replace(temporary, archive)
finally:
    if os.path.exists(temporary):
        os.unlink(temporary)
print(archive)
PY
