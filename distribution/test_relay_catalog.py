#!/usr/bin/env python3
"""Offline catalog preparation tests; gh is mocked and no network is used."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts/prepare-relay-release.py'
spec = importlib.util.spec_from_file_location('prepare_catalog', SCRIPT)
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)


class CatalogTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='relay-catalog-test-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.assets = self.root / 'assets'
        self.assets.mkdir()
        self.output = self.root / 'catalog'
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        gh = self.tools / 'gh'
        gh.write_text('''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
args = sys.argv[1:]
if args[:3] == ['release', 'view', 'v1.2.3']:
    print(json.dumps({'assets': [{'name': p.name} for p in pathlib.Path(os.environ['FIXTURE_ASSETS']).iterdir()]}))
    sys.exit(0)
assert args[:3] == ['release', 'download', 'v1.2.3']
assert args[args.index('--repo') + 1] == 'perapp/herdr-webapp'
out = pathlib.Path(args[args.index('--dir') + 1])
for i, arg in enumerate(args):
    if arg == '--pattern':
        shutil.copyfile(pathlib.Path(os.environ['FIXTURE_ASSETS']) / args[i + 1], out / args[i + 1])
''')
        gh.chmod(0o700)
        self.environment = patch.dict(os.environ, {'PATH': str(self.tools) + ':' + os.environ['PATH'], 'FIXTURE_ASSETS': str(self.assets)})
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.release()

    def release(self, extra=None, missing=None, version='1.2.3'):
        host = prepare.native_target()
        for target in prepare.TARGETS:
            binary = (f'#!/bin/sh\nprintf "herdr-webapp {version}\\n"\n'.encode() if target == host else b'not native: must never execute\n')
            archive = self.assets / f'herdr-webapp-v1.2.3-{target}.tar.gz'
            with tarfile.open(archive, 'w:gz', format=tarfile.USTAR_FORMAT) as tar:
                files = {'herdr-webapp': binary, 'pi-extension/commands.ts': b'// fixture', 'scripts/install-pi-extension.sh': b'not executed', 'LICENSE': b'license'}
                for name, data in files.items():
                    if name == missing:
                        continue
                    entry = tarfile.TarInfo(name)
                    entry.size = len(data)
                    tar.addfile(entry, io.BytesIO(data))
                if extra is not None:
                    tar.addfile(extra, io.BytesIO(b'x' * extra.size) if extra.isfile() else None)
        (self.assets / 'install.sh').write_text('must never execute')
        self.checksums()

    def checksums(self):
        (self.assets / 'SHA256SUMS').write_text(''.join(f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n' for p in sorted(self.assets.iterdir()) if p.name != 'SHA256SUMS'))

    def run_prepare(self, success=True):
        result = subprocess.run(['python3', str(SCRIPT), '--version', '1.2.3', '--output', str(self.output)], capture_output=True, timeout=10)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        self.assertFalse(list(self.root.glob('.relay-prepare-*')))
        return result

    def test_catalog_contains_only_verified_raw_binaries(self):
        self.run_prepare()
        manifest = json.loads((self.output / 'manifest.json').read_text())
        self.assertEqual(set(manifest), {'schemaVersion', 'protocolVersion', 'version', 'assets'})
        self.assertEqual(manifest['version'], '1.2.3')
        self.assertEqual(manifest['schemaVersion'], 1)
        self.assertEqual(manifest['protocolVersion'], 1)
        self.assertEqual({a['target'] for a in manifest['assets']}, set(prepare.TARGETS))
        for asset in manifest['assets']:
            self.assertEqual(asset['url'], f"/client/1.2.3/{asset['target']}/herdr-webapp")
            path = self.output / '1.2.3' / asset['target'] / 'herdr-webapp'
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), asset['sha256'])
            self.assertEqual(path.stat().st_size, asset['size'])
        self.assertEqual(len(list(self.output.rglob('herdr-webapp'))), 8)
        self.assertFalse(list(self.output.rglob('install.sh')))
        self.assertEqual(self.output.stat().st_mode & 0o077, 0)

    def test_legacy_exact_cohort_still_prepares(self):
        for target in set(prepare.TARGETS) - set(prepare.LEGACY_TARGETS):
            (self.assets / f'herdr-webapp-v1.2.3-{target}.tar.gz').unlink()
        self.checksums()
        self.run_prepare()
        manifest = json.loads((self.output / 'manifest.json').read_text())
        self.assertEqual({a['target'] for a in manifest['assets']}, set(prepare.LEGACY_TARGETS))

    def test_partial_or_extra_advertised_assets_fail_before_execution(self):
        archive = self.assets / 'herdr-webapp-v1.2.3-i686-unknown-linux-gnu.tar.gz'
        saved = archive.read_bytes()
        archive.unlink()
        self.checksums()
        self.run_prepare(False)
        self.assertFalse(self.output.exists())
        archive.write_bytes(saved)
        for name in ('other', 'herdr-webapp-v1.2.3-x86_64-pc-windows-msvc.zip'):
            extra = self.assets / name
            extra.write_bytes(b'not approved')
            self.checksums()
            self.run_prepare(False)
            self.assertFalse(self.output.exists())
            extra.unlink()

    def test_new_host_cannot_probe_legacy_release(self):
        for target in set(prepare.TARGETS) - set(prepare.LEGACY_TARGETS):
            (self.assets / f'herdr-webapp-v1.2.3-{target}.tar.gz').unlink()
        self.checksums()
        with patch.object(prepare, 'native_target', return_value='x86_64-apple-darwin'):
            with self.assertRaisesRegex(prepare.InvalidRelease, 'no binary'):
                prepare.prepare('1.2.3', self.output)
        self.assertFalse(self.output.exists())

    def test_partial_release_preserves_previous_catalog(self):
        self.run_prepare()
        before = (self.output / 'manifest.json').read_bytes()
        (self.assets / 'herdr-webapp-v1.2.3-i686-unknown-linux-gnu.tar.gz').unlink()
        self.checksums()
        self.run_prepare(False)
        self.assertEqual((self.output / 'manifest.json').read_bytes(), before)

    def test_bad_checksum_never_activates(self):
        (self.assets / 'install.sh').write_text('tampered')
        self.run_prepare(False)
        self.assertFalse(self.output.exists())

    def test_checksum_allowlist_and_duplicates(self):
        sums = self.assets / 'SHA256SUMS'
        original = sums.read_text()
        for extra in (original.splitlines()[0], '0' * 64 + '  ../escape', '0' * 64 + '  other'):
            with self.subTest(extra=extra):
                sums.write_text(original + extra + '\n')
                self.run_prepare(False)
                self.assertFalse(self.output.exists())
        sums.write_text('\n'.join(original.splitlines()[:-1]) + '\n')
        self.run_prepare(False)

    def test_malicious_archive(self):
        entries = []
        for name in ('../escape', '/absolute', './herdr-webapp', 'unapproved', 'herdr-webapp'):
            entries.append(tarfile.TarInfo(name))
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.DIRTYPE, tarfile.FIFOTYPE):
            entry = tarfile.TarInfo('LICENSE')
            entry.type, entry.linkname = kind, '/etc/passwd'
            entries.append(entry)
        for entry in entries:
            with self.subTest(name=entry.name, kind=entry.type):
                self.release(extra=entry)
                self.run_prepare(False)
                self.assertFalse(self.output.exists())
        self.assertFalse((self.root / 'escape').exists())

    def test_missing_required_member(self):
        self.release(missing='pi-extension/commands.ts')
        self.run_prepare(False)

    def test_mismatched_native_version(self):
        self.release(version='9.9.9')
        self.run_prepare(False)
        self.assertFalse(self.output.exists())

    def test_preserves_previous_on_failure_and_never_overwrites_same_version(self):
        self.run_prepare()
        before = (self.output / 'manifest.json').read_bytes()
        self.run_prepare(False)
        self.assertEqual((self.output / 'manifest.json').read_bytes(), before)
        self.release(version='9.9.9')
        self.run_prepare(False)
        self.assertEqual((self.output / 'manifest.json').read_bytes(), before)

    def test_unmanaged_symlink_and_unsafe_outputs(self):
        self.output.mkdir()
        self.run_prepare(False)
        self.output.rmdir()
        other = self.root / 'other'
        other.mkdir()
        self.output.symlink_to(other, target_is_directory=True)
        self.run_prepare(False)
        self.output.unlink()
        self.run_prepare()
        (self.output / 'manifest.json').chmod(0o666)
        self.run_prepare(False)

    def test_stable_version_only(self):
        for version in ('v1.2.3', '1.2.3-rc1', '01.2.3', '../escape', '1.2'):
            with self.assertRaises(prepare.InvalidRelease):
                prepare.prepare(version, self.output)

    def test_publish_failure_rolls_back_new_version(self):
        self.run_prepare()
        before = (self.output / 'manifest.json').read_bytes()
        with tempfile.TemporaryDirectory(dir=self.root) as tmp:
            stage = Path(tmp)
            (stage / '2.0.0').mkdir()
            (stage / 'manifest.json').write_text('{}')
            with patch.object(prepare.os, 'replace', side_effect=OSError('injected')):
                with self.assertRaises(OSError):
                    prepare.publish(stage, self.output, '2.0.0')
        self.assertEqual((self.output / 'manifest.json').read_bytes(), before)
        self.assertFalse((self.output / '2.0.0').exists())
        self.assertFalse(list(self.output.glob('.manifest-*')))


if __name__ == '__main__':
    unittest.main()
