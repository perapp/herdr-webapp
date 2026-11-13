#!/usr/bin/env python3
"""Prepare a private, verified raw-binary catalog using authenticated gh; no deployment.

Only a complete, validated release becomes visible through manifest.json. Downloads,
installer scripts and extension files are never published or executed.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile

TARGETS = ('x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'aarch64-apple-darwin')
FILES = {'herdr-webapp', 'pi-extension/commands.ts', 'scripts/install-pi-extension.sh'}
MAX_FILE = 128 * 1024 * 1024
MAX_TOTAL = 256 * 1024 * 1024
MARKER = '.relay-catalog-managed'
MARKER_DATA = b'herdr-webapp relay catalog v1\n'


class InvalidRelease(Exception):
    pass


def require(ok, message):
    if not ok:
        raise InvalidRelease(message)


def safe(path, directory=False, private=False):
    info = path.lstat()
    require((stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode)), 'unsafe catalog entry type')
    require(info.st_uid == os.geteuid() and info.st_mode & (0o077 if private else 0o022) == 0,
            'unsafe catalog ownership or permissions')
    require(directory or info.st_nlink == 1, 'hard-linked files are not allowed')
    return info


def read_bounded(path, maximum):
    info = safe(path)
    require(info.st_size <= maximum, 'release file is too large')
    with path.open('rb') as stream:
        data = stream.read(maximum + 1)
    require(len(data) <= maximum, 'release file is too large')
    return data


def check_path(path):
    # Do not silently resolve symlinked ancestors, even if they currently look safe.
    require('..' not in path.parts, 'parent path components are not allowed')
    current = Path(path.anchor)
    for part in path.parts[1:]:
        current /= part
        require(not current.is_symlink(), 'symlinked output paths are not allowed')


def native_target():
    machine = platform.machine().lower()
    if platform.system() == 'Linux':
        # GNU builds must not be executed on a musl host.
        if platform.libc_ver()[0] != 'glibc':
            return None
        return {'x86_64': TARGETS[0], 'aarch64': TARGETS[1], 'arm64': TARGETS[1]}.get(machine)
    if platform.system() == 'Darwin' and machine in ('arm64', 'aarch64'):
        return TARGETS[2]
    return None


def download(version, directory):
    names = {f'herdr-webapp-v{version}-{t}.tar.gz' for t in TARGETS} | {'install.sh'}
    args = ['gh', 'release', 'download', 'v' + version, '--repo', 'perapp/herdr-webapp', '--dir', str(directory)]
    for name in sorted(names | {'SHA256SUMS'}):
        args.extend(['--pattern', name])
    # gh may print private repository URLs or credential diagnostics: never forward it.
    result = subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=180)
    require(result.returncode == 0, 'authenticated release download failed')
    require({p.name for p in directory.iterdir()} == names | {'SHA256SUMS'}, 'unexpected download entries')
    sums = read_bounded(directory / 'SHA256SUMS', 8192).decode('ascii')
    digests = {}
    for line in sums.splitlines():
        match = re.fullmatch(r'([0-9a-f]{64})  ([A-Za-z0-9_.-]+)', line)
        require(match is not None, 'invalid checksum manifest')
        digest, name = match.groups()
        require(name in names and name not in digests, 'unexpected or duplicate checksum entry')
        digests[name] = digest
    require(set(digests) == names, 'incomplete checksum manifest')
    total = 0
    for name in sorted(names):
        data = read_bounded(directory / name, MAX_FILE if name != 'install.sh' else 1024 * 1024)
        total += len(data)
        require(total <= MAX_TOTAL, 'download exceeds total size limit')
        require(hashlib.sha256(data).hexdigest() == digests[name], 'release checksum mismatch')


def unpack(archive, output):
    seen, binary, total = set(), None, 0
    with tarfile.open(archive, 'r:gz') as tar:
        # Stream iteration rather than getmembers: bound metadata as well as bytes.
        for member in tar:
            require(member.name in FILES | {'LICENSE'} and member.name not in seen,
                    'unexpected or duplicate archive member')
            require(member.isfile() and not member.issparse() and not member.pax_headers,
                    'archive links, directories and extended metadata are forbidden')
            require(0 <= member.size <= MAX_FILE, 'archive member exceeds size limit')
            total += member.size
            require(total <= MAX_FILE, 'archive exceeds expanded size limit')
            seen.add(member.name)
            stream = tar.extractfile(member)
            require(stream is not None, 'unreadable archive member')
            # Only retain the binary, but consume every allowed member within its bound.
            with stream:
                data = stream.read(member.size + 1)
            require(len(data) == member.size, 'truncated archive member')
            if member.name == 'herdr-webapp':
                require(len(data) > 0, 'empty binary')
                binary = data
    require(FILES <= seen and binary is not None, 'incomplete release archive')
    output.write_bytes(binary)
    output.chmod(0o700)
    return len(binary), hashlib.sha256(binary).hexdigest()


def validate_managed(root):
    safe(root, directory=True, private=True)
    require(read_bounded(root / MARKER, 128) == MARKER_DATA, 'output is not a managed catalog')
    if (root / 'manifest.json').exists() or (root / 'manifest.json').is_symlink():
        read_bounded(root / 'manifest.json', 65536)


def publish(stage, output, version):
    installed = False
    manifest_tmp = None
    try:
        try:
            output.mkdir(mode=0o700)
            (output / MARKER).write_bytes(MARKER_DATA)
            (output / MARKER).chmod(0o600)
        except FileExistsError:
            validate_managed(output)
        validate_managed(output)
        # Lock the permanent marker inode; do not unlink locks another process uses.
        with (output / MARKER).open('rb') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            validate_managed(output)
            destination = output / version
            require(not destination.exists() and not destination.is_symlink(), 'release version already exists; refusing overwrite')
            # Everything that can fail validation has completed before changing the root.
            try:
                os.rename(stage / version, destination)
                installed = True
                fd, name = tempfile.mkstemp(prefix='.manifest-', dir=output)
                manifest_tmp = Path(name)
                with os.fdopen(fd, 'wb') as stream:
                    stream.write((stage / 'manifest.json').read_bytes())
                    stream.flush()
                    os.fsync(stream.fileno())
                os.replace(manifest_tmp, output / 'manifest.json')
                manifest_tmp = None
                installed = False  # Committed: keep the new version.
            finally:
                if manifest_tmp is not None:
                    manifest_tmp.unlink(missing_ok=True)
                if installed:
                    shutil.rmtree(destination)
    except BaseException:
        # Keep an initially created, empty managed root on publication failure.
        # Removing it here could delete a catalog another preparer just committed.
        # Without manifest.json it never advertises a partially prepared release.
        raise


def prepare(version, output):
    previous_umask = os.umask(0o077)
    try:
        return _prepare(version, output)
    finally:
        os.umask(previous_umask)


def _prepare(version, output):
    require(re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version) is not None and len(version) <= 64,
            'version must be stable x.y.z without a v prefix')
    output = Path(os.path.abspath(output))
    check_path(output)
    require(output.parent.is_dir(), 'output parent must already exist')
    parent = output.parent.stat()
    require(parent.st_uid == os.geteuid() and parent.st_mode & 0o022 == 0,
            'output parent must be owned and not group/world writable')
    if output.exists() or output.is_symlink():
        validate_managed(output)
    with tempfile.TemporaryDirectory(prefix='.relay-prepare-', dir=output.parent) as temporary:
        stage = Path(temporary)
        downloads = stage / 'downloads'
        downloads.mkdir(mode=0o700)
        download(version, downloads)
        assets, total = [], 0
        host = native_target()
        require(host is not None, 'preparation requires a supported native GNU/Linux or Apple Silicon host')
        for target in TARGETS:
            target_dir = stage / version / target
            target_dir.mkdir(parents=True, mode=0o700)
            binary = target_dir / 'herdr-webapp'
            size, digest = unpack(downloads / f'herdr-webapp-v{version}-{target}.tar.gz', binary)
            total += size
            require(total <= MAX_TOTAL, 'catalog exceeds total size limit')
            assets.append({'target': target, 'url': f'/client/{version}/{target}/herdr-webapp', 'size': size, 'sha256': digest})
        # All three archives and their digests are verified before any binary executes.
        probe = stage / version / host / 'herdr-webapp'
        with tempfile.TemporaryFile() as log:
            result = subprocess.run([str(probe), '--version'], stdout=log, stderr=subprocess.DEVNULL, timeout=15)
            require(log.tell() <= 1024, 'binary version output is too large')
            log.seek(0)
            got = log.read(1025)
        require(result.returncode == 0 and got in (f'herdr-webapp {version}\n'.encode(), f'herdr-webapp {version}'.encode()),
                'native binary version does not match requested release')
        (stage / 'manifest.json').write_text(json.dumps({'schemaVersion': 1, 'protocolVersion': 1, 'version': version, 'assets': assets}) + '\n')
        publish(stage, output, version)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    try:
        prepare(args.version, args.output)
    except (InvalidRelease, OSError, ValueError, tarfile.TarError, UnicodeError, subprocess.SubprocessError):
        # Intentionally no raw exception: paths and gh output are private.
        parser.exit(1, 'Release catalog preparation failed; existing catalog was not activated or replaced.\n')
    print('Verified release catalog prepared.')


if __name__ == '__main__':
    main()
