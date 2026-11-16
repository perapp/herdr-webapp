#!/usr/bin/env python3
"""Tagged-release matrix, target execution checks and complete asset manifests.

No installation, Core manipulation, downloads or publishing. Main's three-target
native publisher deliberately does not use this larger release matrix.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import struct
import subprocess

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / 'distribution/release-targets.json'


def matrix(path=MANIFEST):
    rows = json.loads(path.read_text())
    if not isinstance(rows, list) or not rows:
        raise ValueError('empty release matrix')
    seen = set()
    for row in rows:
        if not isinstance(row, dict) or set(row) != {'target', 'runner', 'toolchain', 'execute'}:
            raise ValueError('invalid release matrix row')
        target = row['target']
        if not isinstance(target, str) or not re.fullmatch(r'[a-z0-9_]+-(?:unknown-linux-(?:gnu|gnueabihf|musl)|apple-darwin)', target) or target in seen:
            raise ValueError('invalid or duplicate release target')
        if row['runner'] not in {'ubuntu-22.04', 'ubuntu-24.04-arm', 'macos-15'} or row['toolchain'] not in {'native', 'aarch64-gnu', 'armv7-gnu', 'i686-gnu', 'musl', 'darwin-x86'} or row['execute'] not in {'native', 'qemu-aarch64', 'qemu-arm', 'rosetta'}:
            raise ValueError('unrecognized release build environment')
        seen.add(target)
    return rows


def version():
    text = (ROOT / 'Cargo.toml').read_text()
    package = re.search(r'(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)', text)
    return re.search(r'^version\s*=\s*"([^"]+)"\s*$', package[1], re.M)[1]


def verify_binary(binary, target, expected, revision=None):
    row = next(row for row in matrix() if row['target'] == target)
    binary = binary.resolve(strict=True)
    prefix = {
        'native': [], 'rosetta': ['arch', '-x86_64'],
        'qemu-aarch64': ['qemu-aarch64', '-L', '/usr/aarch64-linux-gnu'],
        'qemu-arm': ['qemu-arm', '-L', '/usr/arm-linux-gnueabihf'],
    }[row['execute']]
    with binary.open('rb') as stream:
        data = stream.read(32)
    machine = {
        'x86_64': (2, 62), 'aarch64': (2, 183), 'armv7': (1, 40), 'i686': (1, 3),
    }[target.split('-')[0]]
    if '-linux-' in target:
        if len(data) < 20 or data[:4] != b'\x7fELF' or data[4:6] != bytes([machine[0], 1]) or struct.unpack_from('<H', data, 18)[0] != machine[1]:
            raise ValueError('release binary ELF architecture mismatch')
    else:
        cpu = 0x01000007 if target.startswith('x86_64-') else 0x0100000C
        if len(data) < 8 or data[:4] != b'\xcf\xfa\xed\xfe' or struct.unpack_from('<I', data, 4)[0] != cpu:
            raise ValueError('release binary Mach-O architecture mismatch')
    if target.endswith('-musl'):
        headers = subprocess.check_output(['readelf', '-lW', str(binary)], text=True)
        dynamic = subprocess.check_output(['readelf', '-dW', str(binary)], text=True)
        if 'INTERP' in headers or '(NEEDED)' in dynamic:
            raise ValueError('musl release must be static, without an ELF interpreter or shared libraries')
    got = subprocess.check_output([*prefix, str(binary), '--version'], text=True, timeout=60).strip()
    if got != f'herdr-webapp {expected}':
        raise ValueError('release target executable version mismatch')
    if revision is not None:
        if not re.fullmatch(r'[0-9a-f]{40}', revision):
            raise ValueError('invalid verified tag revision')
        actual = subprocess.check_output([*prefix, str(binary), '--build-revision'], text=True, timeout=60).strip()
        display = subprocess.check_output([*prefix, str(binary), '--build-version'], text=True, timeout=60).strip()
        if actual != revision or display != f'herdr-webapp {expected}':
            raise ValueError('release executable is not the clean verified tag revision')
    print(f'Verified {target}: {row["execute"]} execution' + ('; static musl linkage' if target.endswith('-musl') else ''))


def assemble(directory, expected_version):
    if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', expected_version):
        raise ValueError('release version must be stable x.y.z')
    expected = {f'herdr-webapp-v{expected_version}-{row["target"]}.tar.gz' for row in matrix()}
    if {p.name for p in directory.iterdir()} != expected or any(p.is_symlink() or not p.is_file() for p in directory.iterdir()):
        raise ValueError('incomplete or unexpected release platform manifest')
    shutil.copyfile(ROOT / 'distribution/install.sh', directory / 'install.sh')
    assets = sorted(expected | {'install.sh'})
    (directory / 'SHA256SUMS').write_text(''.join(
        f'{hashlib.sha256((directory / name).read_bytes()).hexdigest()}  {name}\n' for name in assets))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('matrix')
    verify = commands.add_parser('verify-binary')
    verify.add_argument('--binary', type=Path, required=True)
    verify.add_argument('--target', required=True)
    verify.add_argument('--version')
    verify.add_argument('--revision')
    assets = commands.add_parser('assemble')
    assets.add_argument('--directory', type=Path, required=True)
    assets.add_argument('--version', required=True)
    args = parser.parse_args()
    if args.command == 'matrix':
        print(json.dumps({'include': matrix()}, separators=(',', ':')))
    elif args.command == 'verify-binary':
        verify_binary(args.binary, args.target, args.version or version(), args.revision)
    else:
        assemble(args.directory, args.version)


if __name__ == '__main__':
    main()
