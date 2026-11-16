#!/usr/bin/env python3
"""Offline release/installer security tests; all writes stay inside TemporaryDirectory."""
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
INSTALLER = ROOT / 'distribution/install.sh'
TARGET = 'x86_64-unknown-linux-gnu'


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='herdr-release-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        self.assets = self.root / 'assets'
        self.assets.mkdir()
        self.install = self.root / 'data/herdr-webapp'
        self.bin = self.root / 'bin'
        self.env = dict(os.environ, HOME=str(self.root),
                        HERDR_WEB_INSTALL_DIR=str(self.install),
                        HERDR_WEB_BIN_DIR=str(self.bin),
                        FIXTURE_ASSETS=str(self.assets),
                        PATH=f'{self.tools}:' + os.environ['PATH'])
        self.env.pop('HERDR_WEB_VERSION', None)
        self.env.pop('XDG_DATA_HOME', None)
        self.platform()
        # Mock only the pointer-width probe; archive validation, path checks,
        # Core setup, and receipt handling all run through the real Python.
        self.tool('python3', '#!/bin/sh\nif [ "${1:-}" = -c ] && [ "${2:-}" = \'import struct; print(struct.calcsize("P") * 8)\' ]; then printf "%s\\n" "$FIXTURE_BITS"; else exec ' + sys.executable + ' "$@"; fi\n')
        self.tool('gh', '''#!/usr/bin/env python3
import os, pathlib, shutil, sys
args = sys.argv[1:]
if args[:2] == ['auth', 'status']:
    sys.exit(0)
if args[0] == 'api':
    endpoint = args[3]
    if '/tags/' in endpoint:
        print(endpoint.rsplit('/', 1)[1])
    else:
        print('v' + os.environ.get('FIXTURE_VERSION', '0.0.1'))
    sys.exit(0)
if args[:2] == ['release', 'download']:
    dest = pathlib.Path(args[args.index('--dir') + 1])
    for i, arg in enumerate(args):
        if arg == '--pattern':
            shutil.copyfile(pathlib.Path(os.environ['FIXTURE_ASSETS']) / args[i+1], dest / args[i+1])
    sys.exit(0)
sys.exit('unexpected gh call')
''')
        self.make_release()

    def tool(self, name, contents):
        path = self.tools / name
        path.write_text(contents)
        path.chmod(0o755)

    def platform(self, system='Linux', machine='x86_64', bits=64, libc='glibc 2.35'):
        self.env['FIXTURE_BITS'] = str(bits)
        self.tool('uname', f'#!/bin/sh\ncase "$1" in -s) echo {system};; -m) echo {machine};; esac\n')
        self.tool('getconf', '#!/bin/sh\n' + (f'echo "{libc}"\n' if libc.startswith('glibc ') else 'exit 1\n'))
        self.tool('ldd', '#!/bin/sh\necho "' + ('musl libc' if libc == 'musl' else 'unknown libc') + '" >&2\nexit 1\n')

    def make_release(self, version='0.0.1', binary_version=None, extra=None, target=TARGET):
        name = f'herdr-webapp-v{version}-{target}.tar.gz'
        path = self.assets / name
        with tarfile.open(path, 'w:gz') as tar:
            files = {'herdr-webapp': f'#!/bin/sh\nprintf "herdr-webapp {binary_version or version}\\n"\n'.encode(),
                     'pi-extension/commands.ts': b'// fixture\n',
                     'scripts/install-pi-extension.sh': b'#!/bin/sh\nexit 0\n'}
            for key, data in files.items():
                info = tarfile.TarInfo(key)
                info.size = len(data)
                info.mode = 0o755
                tar.addfile(info, io.BytesIO(data))
            if extra:
                tar.addfile(extra)
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        (self.assets / 'SHA256SUMS').write_text(f'{digest}  {name}\n')
        self.env['FIXTURE_VERSION'] = version

    def run_installer(self, success=True, pipe=False):
        args = ['sh', '-s', '--', '--no-install-core'] if pipe else ['sh', str(INSTALLER), '--no-install-core']
        result = subprocess.run(args, env=self.env, text=True,
                                input=INSTALLER.read_text() if pipe else '',
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        lock = Path(str(self.install) + '.update-lock')
        if not getattr(self, 'expect_lock', False):
            self.assertFalse(lock.exists())
        if self.install.parent.exists():
            self.assertEqual(list(self.install.parent.glob('.herdr-webapp-install.*')), [])
        return result

    def test_pipe_install_update_receipt_and_preserve_state(self):
        self.run_installer(pipe=True)
        self.assertEqual(json.loads((self.install / 'install.json').read_text()), {
            'schemaVersion': 1, 'repository': 'perapp/herdr-webapp', 'target': TARGET, 'version': '0.0.1'})
        self.assertEqual((self.bin / 'herdr-webapp').readlink(), self.install / 'herdr-webapp')
        self.assertEqual(self.install.stat().st_mode & 0o777, 0o700)
        self.assertEqual((self.install / 'install.json').stat().st_mode & 0o777, 0o600)
        state = self.root / '.local/state/herdr-webapp/auth'
        state.parent.mkdir(parents=True)
        state.write_text('private-state-fixture')
        self.make_release('0.0.2')
        self.env['HERDR_WEB_VERSION'] = '0.0.2'
        self.run_installer()
        self.assertEqual(json.loads((self.install / 'install.json').read_text())['version'], '0.0.2')
        self.assertEqual(state.read_text(), 'private-state-fixture')
        self.assertFalse((self.root / '.pi').exists())

    def test_all_published_target_mappings(self):
        cases = [
            ('Linux', 'x86_64', 64, 'glibc 2.35', 'x86_64-unknown-linux-gnu'),
            ('Linux', 'aarch64', 64, 'glibc 2.35', 'aarch64-unknown-linux-gnu'),
            ('Linux', 'armv7l', 32, 'glibc 2.35', 'armv7-unknown-linux-gnueabihf'),
            ('Linux', 'i686', 32, 'glibc 2.35', 'i686-unknown-linux-gnu'),
            ('Linux', 'x86_64', 64, 'musl', 'x86_64-unknown-linux-musl'),
            ('Linux', 'aarch64', 64, 'musl', 'aarch64-unknown-linux-musl'),
            ('Darwin', 'arm64', 64, 'unknown', 'aarch64-apple-darwin'),
            ('Darwin', 'x86_64', 64, 'unknown', 'x86_64-apple-darwin'),
        ]
        for system, machine, bits, libc, target in cases:
            with self.subTest(target=target):
                self.platform(system, machine, bits, libc)
                self.make_release(target=target)
                self.install = self.root / target / 'app'
                self.bin = self.root / (target + '-bin')
                self.env.update(HERDR_WEB_INSTALL_DIR=str(self.install), HERDR_WEB_BIN_DIR=str(self.bin))
                result = self.run_installer()
                self.assertIn(target, result.stdout)
                self.assertEqual(json.loads((self.install / 'install.json').read_text())['target'], target)

    def test_32_bit_userland_on_64_bit_kernel(self):
        for machine, target in [('x86_64', 'i686-unknown-linux-gnu'),
                                ('aarch64', 'armv7-unknown-linux-gnueabihf')]:
            with self.subTest(machine=machine):
                self.platform(machine=machine, bits=32)
                self.make_release(target=target)
                self.install = self.root / machine / 'app'
                self.bin = self.root / (machine + '-bin')
                self.env.update(HERDR_WEB_INSTALL_DIR=str(self.install), HERDR_WEB_BIN_DIR=str(self.bin))
                self.run_installer()
                self.assertEqual(json.loads((self.install / 'install.json').read_text())['target'], target)

    def test_unsupported_architectures_and_libcs_fail_before_download(self):
        cases = [('Linux', 'riscv64', 64, 'glibc 2.35', 'unsupported platform'),
                 ('Linux', 'armv6l', 32, 'glibc 2.35', 'unsupported platform'),
                 ('Linux', 'x86_64', 32, 'musl', 'no published musl build'),
                 ('Linux', 'armv7l', 32, 'musl', 'no published musl build'),
                 ('Linux', 'x86_64', 64, 'unknown', 'cannot identify Linux libc'),
                 ('Linux', 'x86_64', 64, 'glibc 2.34', 'glibc 2.35 or newer'),
                 ('Darwin', 'x86_64', 32, 'unknown', 'unsupported platform'),
                 ('FreeBSD', 'x86_64', 64, 'unknown', 'unsupported platform')]
        self.tool('gh', '#!/bin/sh\nprintf "unexpected download" >&2\nexit 1\n')
        for system, machine, bits, libc, message in cases:
            with self.subTest(system=system, machine=machine, bits=bits, libc=libc):
                self.platform(system, machine, bits, libc)
                result = self.run_installer(False)
                self.assertIn(message, result.stderr)
                self.assertNotIn('unexpected download', result.stderr)
                self.assertFalse(self.install.exists())

    def test_receipt_target_mismatch_is_not_overwritten(self):
        self.run_installer()
        receipt = self.install / 'install.json'
        original = receipt.read_bytes()
        self.platform(machine='x86_64', bits=64, libc='musl')
        self.make_release(target='x86_64-unknown-linux-musl')
        result = self.run_installer(False)
        self.assertIn('refusing unmanaged installation', result.stderr)
        self.assertEqual(receipt.read_bytes(), original)

    def test_unmanaged_directory(self):
        self.install.mkdir(parents=True)
        (self.install / 'keep').write_text('keep')
        self.run_installer(False)
        self.assertEqual((self.install / 'keep').read_text(), 'keep')

    def test_unmanaged_cli_file_and_symlink(self):
        self.bin.mkdir()
        link = self.bin / 'herdr-webapp'
        link.write_text('keep')
        self.run_installer(False)
        self.assertEqual(link.read_text(), 'keep')
        link.unlink()
        link.symlink_to('/unmanaged-target')
        self.run_installer(False)
        self.assertEqual(str(link.readlink()), '/unmanaged-target')

    def test_lock_never_deleted(self):
        lock = Path(str(self.install) + '.update-lock')
        lock.mkdir(parents=True)
        self.expect_lock = True
        self.run_installer(False)
        self.assertTrue(lock.is_dir())

    def test_live_pid_blocks_update(self):
        self.run_installer()
        pid = Path(str(self.install) + '.pid')
        pid.write_text(str(os.getpid()))
        result = self.run_installer(False)
        self.assertIn('UI updater', result.stderr)
        self.assertEqual(pid.read_text(), str(os.getpid()))

    def test_corrupt_download_leaves_old_installation(self):
        self.run_installer()
        old = (self.install / 'herdr-webapp').read_bytes()
        self.make_release('0.0.2')
        with (self.assets / f'herdr-webapp-v0.0.2-{TARGET}.tar.gz').open('ab') as f:
            f.write(b'corruption')
        self.run_installer(False)
        self.assertEqual((self.install / 'herdr-webapp').read_bytes(), old)

    def test_version_mismatch(self):
        self.make_release(binary_version='9.9.9')
        self.run_installer(False)
        self.assertFalse(self.install.exists())

    def test_bad_version(self):
        self.env['HERDR_WEB_VERSION'] = '../bad'
        self.run_installer(False)
        self.assertFalse(self.install.exists())

    def test_malicious_archive(self):
        for name, kind in [('../escape', tarfile.REGTYPE), ('/escape', tarfile.REGTYPE),
                           ('herdr-webapp', tarfile.REGTYPE), ('LICENSE', tarfile.SYMTYPE),
                           ('LICENSE', tarfile.LNKTYPE)]:
            with self.subTest(name=name, kind=kind):
                extra = tarfile.TarInfo(name)
                extra.type = kind
                extra.linkname = '/tmp/never-extract'
                self.make_release(extra=extra)
                self.run_installer(False)
                self.assertFalse(self.install.exists())

    def test_failed_swap_restores_backup(self):
        self.run_installer()
        old = (self.install / 'install.json').read_bytes()
        self.make_release('0.0.2')
        # Fail only the payload rename; backup and restoration use the real mv.
        self.tool('mv', '#!/bin/sh\ncase "$2" in */payload) exit 1;; esac\nexec /bin/mv "$@"\n')
        self.run_installer(False)
        self.assertEqual((self.install / 'install.json').read_bytes(), old)

    def test_failed_cli_swap_restores_backup(self):
        import sys
        self.run_installer()
        old = (self.install / 'install.json').read_bytes()
        self.make_release('0.0.2')
        self.tool('python3', '#!/bin/sh\ncase "${2:-}" in */.herdr-webapp-link.*/herdr-webapp) exit 1;; esac\nexec ' + sys.executable + ' "$@"\n')
        self.run_installer(False)
        self.assertEqual((self.install / 'install.json').read_bytes(), old)
        self.assertEqual((self.bin / 'herdr-webapp').readlink(), self.install / 'herdr-webapp')

    def test_relative_and_symlink_paths(self):
        self.env['HERDR_WEB_INSTALL_DIR'] = 'relative'
        self.run_installer(False)
        real = self.root / 'real'
        real.mkdir()
        alias = self.root / 'alias'
        alias.symlink_to(real)
        self.env['HERDR_WEB_INSTALL_DIR'] = str(alias / 'app')
        self.run_installer(False)


class PackageTests(unittest.TestCase):
    def test_every_release_target_can_be_packaged_and_windows_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix='herdr-package-targets-') as tmp:
            root = Path(tmp)
            version = re.search(r'^version = "([^"]+)"', (ROOT / 'Cargo.toml').read_text(), re.M)[1]
            binary = root / 'binary'
            binary.write_text(f'#!/bin/sh\necho "herdr-webapp {version}"\n')
            binary.chmod(0o755)
            env = dict(os.environ, RELEASE_TAG='v' + version)
            for key in ['GITHUB_REF_TYPE', 'GITHUB_REF_NAME', 'HERDR_WEB_VERSION', 'RELEASE_VERSION']:
                env.pop(key, None)
            targets = json.loads((ROOT / 'distribution/release-targets.json').read_text())
            for entry in targets:
                with self.subTest(target=entry['target']):
                    subprocess.run(['sh', str(ROOT / 'scripts/package-release.sh'), entry['target'], str(binary), str(root / 'out')], env=env, check=True, stdout=subprocess.PIPE)
                    self.assertTrue((root / 'out' / f'herdr-webapp-v{version}-{entry["target"]}.tar.gz').is_file())
            result = subprocess.run(['sh', str(ROOT / 'scripts/package-release.sh'), 'x86_64-pc-windows-msvc', str(binary), str(root / 'out')], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.assertNotEqual(result.returncode, 0)

    def test_deterministic_safe_archive_and_version_checks(self):
        with tempfile.TemporaryDirectory(prefix='herdr-package-test-') as tmp:
            root = Path(tmp)
            manifest = (ROOT / 'Cargo.toml').read_text()
            version = re.search(r'^version = "([^"]+)"', manifest, re.M)[1]
            binary = root / 'binary'
            binary.write_text(f'#!/bin/sh\necho "herdr-webapp {version}"\n')
            binary.chmod(0o755)
            cmd = ['sh', str(ROOT / 'scripts/package-release.sh'), TARGET, str(binary), str(root / 'out')]
            env = dict(os.environ, RELEASE_TAG='v' + version)
            for key in ['GITHUB_REF_TYPE', 'GITHUB_REF_NAME', 'HERDR_WEB_VERSION', 'RELEASE_VERSION']:
                env.pop(key, None)
            subprocess.run(cmd, env=env, check=True, stdout=subprocess.PIPE)
            archive = root / 'out' / f'herdr-webapp-v{version}-{TARGET}.tar.gz'
            first = archive.read_bytes()
            subprocess.run(cmd, env=env, check=True, stdout=subprocess.PIPE)
            self.assertEqual(first, archive.read_bytes())
            with tarfile.open(archive) as tar:
                self.assertIn('herdr-webapp', tar.getnames())
                for member in tar:
                    self.assertTrue(member.isfile())
                    self.assertNotIn('..', Path(member.name).parts)
                    self.assertFalse(Path(member.name).is_absolute())
                    self.assertEqual(member.mtime, 0)
            env['RELEASE_TAG'] = 'v9.9.9'
            result = subprocess.run(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.assertNotEqual(result.returncode, 0)


if __name__ == '__main__':
    unittest.main()
