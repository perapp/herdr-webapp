#!/usr/bin/env python3
"""Install locked frontend dependencies only when inputs/toolchain change."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess

ROOT = Path(__file__).resolve().parent.parent
BINS = ('vite', 'svelte-check', 'eslint', 'vitest')


def fingerprint(frontend, run=subprocess.run):
    digest = hashlib.sha256()
    digest.update((platform.system() + ':' + platform.machine()).encode())
    for path in [frontend / 'package.json', frontend / 'package-lock.json',
                 frontend / '.npmrc', frontend.parent / '.npmrc']:
        digest.update(path.name.encode() + b'\0')
        digest.update(path.read_bytes() if path.exists() else b'<missing>')
    for tool in ('node', 'npm'):
        result = run([tool, '--version'], check=True, capture_output=True)
        digest.update(result.stdout)
    # Configuration values are hashed, never copied into the cache marker/log.
    digest.update(json.dumps({k: v for k, v in os.environ.items()
                              if k.lower().startswith('npm_config_')}, sort_keys=True).encode())
    return digest.hexdigest()


def prepare(frontend, force=False, run=subprocess.run):
    frontend = Path(frontend)
    lock = frontend.parent / 'target/frontend-deps.lock'
    lock.parent.mkdir(parents=True, exist_ok=True)
    with lock.open('a') as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        expected = fingerprint(frontend, run)
        marker = frontend / 'node_modules/.herdr-deps-sha256'
        if (not force and marker.is_file() and marker.read_text().strip() == expected
                and all((frontend / 'node_modules/.bin' / name).is_file() for name in BINS)):
            return False
        marker.unlink(missing_ok=True)
        run(['npm', 'ci'], cwd=frontend, check=True)
        marker.write_text(expected + '\n')
        return True


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--force', action='store_true')
    args = parser.parse_args()
    prepare(ROOT / 'frontend', args.force)
