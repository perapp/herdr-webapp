#!/usr/bin/env python3
"""Native main build stamping and all-or-nothing GitHub Pages site assembly.

publication.json is trusted publisher metadata served by Pages HTTPS, not a
cryptographic attestation. No tags, releases, credentials or network access here.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import stat
import struct
import subprocess
import tempfile
import tomllib

TARGETS = ('x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'aarch64-apple-darwin')
REPOSITORY = 'perapp/herdr-webapp'
WORKFLOW = '.github/workflows/main.yml'
PUBLIC_BASE = 'https://perapp.github.io/herdr-webapp'
MAX_FILE = 128 * 1024 * 1024
MAX_TOTAL = 256 * 1024 * 1024
SHA = re.compile(r'[0-9a-f]{40}')
# Match the existing strict Rust schema-2 catalog contract, not all Cargo SemVer.
VERSION = re.compile(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)')
STAMP_FIELDS = {'schemaVersion', 'repository', 'workflow', 'runId', 'revision', 'version', 'target', 'hostSystem', 'hostMachine', 'size', 'sha256'}
HOSTS = {TARGETS[0]: ('Linux', 'x86_64'), TARGETS[1]: ('Linux', 'aarch64'), TARGETS[2]: ('Darwin', 'arm64')}


class InvalidBuild(ValueError):
    pass


def require(ok, message):
    if not ok:
        raise InvalidBuild(message)


def context(revision, version, run_id):
    require(isinstance(revision, str) and SHA.fullmatch(revision), 'expected full lowercase commit SHA')
    require(isinstance(version, str) and len(version) <= 64 and VERSION.fullmatch(version),
            'catalog requires a plain X.Y.Z Cargo version')
    require(type(run_id) is int and run_id > 0, 'invalid workflow run ID')


def safe(path, directory=False):
    # Never follow a symlink in either the file or any of its ancestors.
    path = Path(os.path.abspath(path))
    for part in (path, *path.parents):
        require(not part.is_symlink(), 'symlink paths forbidden')
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode), 'unsafe entry type')
    require(directory or info.st_nlink == 1, 'hard links forbidden')
    return info


def read(path, maximum):
    info = safe(path)
    require(0 < info.st_size <= maximum, 'invalid file size')
    with Path(path).open('rb') as stream:
        data = stream.read(maximum + 1)
    require(len(data) == info.st_size, 'file changed or exceeded bound')
    return data


def json_bytes(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':')) + '\n').encode('utf-8')


def load_json(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, 'duplicate JSON key')
            result[key] = value
        return result
    return json.loads(read(path, 16384), object_pairs_hook=unique)


def native(target):
    require(target in HOSTS, 'unsupported target')
    require((platform.system(), platform.machine().lower()) == HOSTS[target], 'runner is not the requested native ISA')
    if platform.system() == 'Linux':
        require(platform.libc_ver()[0] == 'glibc', 'GNU build requires glibc')


def binary_identity(data, target):
    if target in TARGETS[:2]:
        require(len(data) >= 64 and data[:7] == b'\x7fELF\x02\x01\x01', 'expected little-endian 64-bit ELF')
        require(struct.unpack_from('<H', data, 18)[0] == (62 if target == TARGETS[0] else 183), 'ELF ISA mismatch')
        require(struct.unpack_from('<H', data, 16)[0] in (2, 3), 'ELF is not executable')
    else:
        require(len(data) >= 32 and data[:4] == b'\xcf\xfa\xed\xfe', 'expected thin 64-bit Mach-O')
        require(struct.unpack_from('<I', data, 4)[0] == 0x0100000C, 'Mach-O ISA mismatch')
        require(struct.unpack_from('<I', data, 12)[0] == 2, 'Mach-O is not executable')


def probe(binary, flag, expected):
    with tempfile.TemporaryFile() as log:
        result = subprocess.run([str(Path(binary).absolute()), flag], stdout=log, stderr=subprocess.DEVNULL, timeout=15)
        require(log.tell() <= 1024, 'identity output exceeds bound')
        log.seek(0)
        require(result.returncode == 0 and log.read(1025) == (expected + '\n').encode(), 'native executable identity mismatch')


def private_stage(output):
    output = Path(os.path.abspath(output))
    safe(output.parent, directory=True)
    require(not output.exists() and not output.is_symlink(), 'refusing existing output')
    return output, tempfile.TemporaryDirectory(prefix='.main-ci-', dir=output.parent)


def stamp(binary, output, target, revision, version, run_id):
    context(revision, version, run_id)
    native(target)
    data = read(binary, MAX_FILE)
    binary_identity(data, target)
    probe(binary, '--version', 'herdr-webapp ' + version)
    probe(binary, '--build-revision', revision)
    require(read(binary, MAX_FILE) == data, 'binary changed during probing')
    metadata = dict(schemaVersion=1, repository=REPOSITORY, workflow=WORKFLOW, runId=run_id,
                    revision=revision, version=version, target=target, hostSystem=HOSTS[target][0],
                    hostMachine=HOSTS[target][1], size=len(data), sha256=hashlib.sha256(data).hexdigest())
    output, temporary = private_stage(output)
    with temporary as folder:
        stage = Path(folder)
        (stage / 'herdr-webapp').write_bytes(data)
        (stage / 'herdr-webapp').chmod(0o700)
        (stage / 'native.json').write_bytes(json_bytes(metadata))
        os.rename(stage, output)


def validate_stamp(metadata, data, target, revision, version, run_id):
    require(type(metadata) is dict and set(metadata) == STAMP_FIELDS, 'invalid native stamp schema')
    expected = dict(schemaVersion=1, repository=REPOSITORY, workflow=WORKFLOW, runId=run_id,
                    revision=revision, version=version, target=target, hostSystem=HOSTS[target][0],
                    hostMachine=HOSTS[target][1], size=len(data), sha256=hashlib.sha256(data).hexdigest())
    require(type(metadata['schemaVersion']) is int and type(metadata['runId']) is int and type(metadata['size']) is int,
            'stamp integers must not be boolean')
    require(metadata == expected, 'native provenance or byte identity mismatch')
    binary_identity(data, target)


def assemble(inputs, output, revision, version, run_id):
    context(revision, version, run_id)
    inputs = Path(inputs)
    safe(inputs, directory=True)
    require({p.name for p in inputs.iterdir()} == {'main-native-' + t for t in TARGETS}, 'incomplete or extra native artifact set')
    # Validate all inputs before exposing any output. Raw bytes remain private until
    # complete; deploy-pages replaces the whole site only after a successful upload.
    assets, total = [], 0
    output, temporary = private_stage(output)
    with temporary as folder:
        stage = Path(folder)
        for target in TARGETS:
            source = inputs / ('main-native-' + target)
            safe(source, directory=True)
            require({p.name for p in source.iterdir()} == {'herdr-webapp', 'native.json'}, 'unexpected native artifact files')
            data = read(source / 'herdr-webapp', MAX_FILE)
            total += len(data)
            require(total <= MAX_TOTAL, 'full set exceeds size bound')
            validate_stamp(load_json(source / 'native.json'), data, target, revision, version, run_id)
            relative = f'main/client/{revision}/{target}/herdr-webapp'
            destination = stage / relative
            destination.parent.mkdir(parents=True, mode=0o700)
            destination.write_bytes(data)
            destination.chmod(0o700)
            assets.append(dict(target=target, url='/' + relative, size=len(data), sha256=hashlib.sha256(data).hexdigest()))
        manifest = json_bytes(dict(schemaVersion=2, protocolVersion=1, channel='main', version=version, revision=revision, assets=assets))
        (stage / 'main/client/manifest.json').write_bytes(manifest)
        publication = dict(schemaVersion=1, repository=REPOSITORY, branch='main', revision=revision,
                           workflow=WORKFLOW, runId=run_id, manifestSha256=hashlib.sha256(manifest).hexdigest())
        (stage / 'main/publication.json').write_bytes(json_bytes(publication))
        os.rename(stage, output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('host', 'stamp', 'assemble'))
    parser.add_argument('--target', choices=TARGETS)
    parser.add_argument('--revision')
    parser.add_argument('--run-id', type=int)
    parser.add_argument('--cargo', type=Path, default=Path('Cargo.toml'))
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--inputs', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    try:
        if args.operation == 'host':
            native(args.target)
            return
        require(args.revision is not None and args.run_id is not None and args.output is not None, 'missing publication context')
        version = tomllib.loads(read(args.cargo, 1024 * 1024).decode())['package']['version']
        if args.operation == 'stamp':
            require(args.binary is not None, 'missing native binary')
            stamp(args.binary, args.output, args.target, args.revision, version, args.run_id)
        else:
            require(args.inputs is not None, 'missing artifact inputs')
            assemble(args.inputs, args.output, args.revision, version, args.run_id)
    except (InvalidBuild, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        parser.exit(1, 'Main CI validation failed; no publication activated.\n')
    print('Complete main CI output verified.')


if __name__ == '__main__':
    main()
