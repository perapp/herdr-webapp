#!/usr/bin/env python3
"""Offline dependency/IPC tests: never connect to the user's Core."""
import importlib.util
import json
import os
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).parent
spec = importlib.util.spec_from_file_location('launcher', ROOT / 'relay-launcher.py')
launcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launcher)
bootstrap_spec = importlib.util.spec_from_file_location('core_bootstrap', ROOT / 'core-bootstrap.py')
bootstrap = importlib.util.module_from_spec(bootstrap_spec)
bootstrap_spec.loader.exec_module(bootstrap)


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='herdr-bootstrap-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = patch.dict(os.environ, {'HOME': str(self.root), 'PATH': '/usr/bin:/bin', 'XDG_RUNTIME_DIR': str(self.root / 'run'), 'XDG_CONFIG_HOME': str(self.root / 'config'), 'XDG_STATE_HOME': str(self.root / 'state')}, clear=True)
        self.env.start()
        self.addCleanup(self.env.stop)
        self.bin = self.root / '.local/bin'
        self.bin.mkdir(parents=True)
        self.calls = []

    def fake_core(self, directory=None, valid=True):
        directory = directory or self.bin
        directory.mkdir(parents=True, exist_ok=True)
        core = directory / 'herdr-core'
        core.write_text('#!/bin/sh\nif [ "$1" = --version ]; then echo "' + ('hc 0.1.42' if valid else 'not a core') + '"; else exit 1; fi\n')
        core.chmod(0o700)
        return core

    def download(self, url, path):
        self.calls.append(url)
        if url.endswith('latest.json'):
            path.write_text(json.dumps({'version': '0.1.42', 'generation': 1}))
        else:
            path.write_text('''#!/bin/sh
[ "$HC_VERSION" = 0.1.42 ] || exit 1
[ ! -e "$HC_INSTALL_DIR/herdr-core" ] || exit 2
printf '#!/bin/sh\necho hc 0.1.42\n' > "$HC_INSTALL_DIR/herdr-core"
chmod 700 "$HC_INSTALL_DIR/herdr-core"
''')

    def test_embedded_sources_match(self):
        def block(name):
            return (ROOT / name).read_text().split('# BEGIN CORE BOOTSTRAP')[1].split('# END CORE BOOTSTRAP')[0]
        self.assertEqual(block('core-bootstrap.py'), block('relay-launcher.py'))
        self.assertEqual(block('core-bootstrap.py'), block('install.sh'))

    def test_release_installer_prepares_running_core_for_standalone_webapp(self):
        self.assertIn("ensure_core(no_install=sys.argv[1] == 'true', start=True)", (ROOT / 'install.sh').read_text())

    def test_missing_core_pinned_official_install_and_path_handoff(self):
        with patch.object(launcher, 'core_download', side_effect=self.download):
            launcher.ensure_core(start=False)
        self.assertEqual(self.calls, ['https://releases.herdr.dev/core/latest.json', 'https://releases.herdr.dev/core/install.sh'])
        self.assertTrue((self.bin / 'herdr-core').is_file())
        self.assertEqual(os.environ['PATH'].split(':')[0], str(self.bin))
        with patch.object(launcher, 'core_download', side_effect=AssertionError('reinstall')):
            launcher.ensure_core(start=False)

    def test_32_bit_missing_core_requires_manual_install_without_download(self):
        for machine in ('i686', 'armv7l', 'x86_64', 'aarch64'):
            with self.subTest(machine=machine), patch.object(bootstrap.os, 'uname', return_value=('Linux', '', '', '', machine)), patch.object(bootstrap.struct, 'calcsize', return_value=4), patch.object(bootstrap, 'core_candidates', return_value=iter(())), patch.object(bootstrap, 'core_download', side_effect=AssertionError('download')):
                with self.assertRaisesRegex(bootstrap.CoreBootstrapError, 'build Core from source'):
                    bootstrap.ensure_core(start=False)
                self.assertFalse((self.bin / '.herdr-web-core-install.lock').exists())

    def test_32_bit_existing_core_and_optout_still_work(self):
        self.fake_core()
        with patch.object(bootstrap.struct, 'calcsize', return_value=4), patch.object(bootstrap, 'core_download', side_effect=AssertionError('download')):
            bootstrap.ensure_core(start=False)
            with patch.object(bootstrap, 'core_hello', side_effect=AssertionError('probe')):
                bootstrap.ensure_core(no_install=True)

    def test_official_core_platforms_allow_bootstrap(self):
        for system, machine in [('Linux', 'x86_64'), ('Linux', 'aarch64'),
                                ('Darwin', 'arm64'), ('Darwin', 'x86_64')]:
            with self.subTest(system=system, machine=machine), patch.object(bootstrap.os, 'uname', return_value=(system, '', '', '', machine)), patch.object(bootstrap.struct, 'calcsize', return_value=8), patch.object(bootstrap, 'core_candidates', return_value=iter(())), patch.object(bootstrap, 'core_download', side_effect=bootstrap.CoreBootstrapError('official download reached')):
                with self.assertRaisesRegex(bootstrap.CoreBootstrapError, 'official download reached'):
                    bootstrap.ensure_core(start=False)
                self.assertFalse((self.bin / '.herdr-web-core-install.lock').exists())

    def test_optout_never_probes_or_installs(self):
        with patch.object(launcher, 'core_hello', side_effect=AssertionError('probe')):
            launcher.ensure_core(no_install=True)

    def test_explicit_endpoint_and_config_skip_local_setup(self):
        os.environ['HC_SOCKET'] = str(self.root / 'remote.sock')
        with patch.object(launcher, 'core_download', side_effect=AssertionError('install')):
            launcher.ensure_core()
        del os.environ['HC_SOCKET']
        config = self.root / 'config/herdr-core'
        config.mkdir(parents=True)
        (config / 'config.toml').write_text('socket = "/custom/core.sock"\n')
        with patch.object(launcher, 'core_download', side_effect=AssertionError('install')):
            launcher.ensure_core()

    def test_existing_core_reused_without_download(self):
        self.fake_core()
        with patch.object(launcher, 'core_download', side_effect=AssertionError('download')):
            launcher.ensure_core(start=False)

    def test_unusable_core_not_overwritten(self):
        self.fake_core(valid=False)
        with self.assertRaisesRegex(launcher.CoreBootstrapError, 'unusable'):
            launcher.ensure_core(start=False)

    def test_incompatible_metadata_refused_before_installer(self):
        def incompatible(url, path):
            self.calls.append(url)
            path.write_text('{"version":"2.0.0","generation":2}')
        with patch.object(launcher, 'core_download', side_effect=incompatible), self.assertRaisesRegex(launcher.CoreBootstrapError, 'generation 1'):
            launcher.ensure_core(start=False)
        self.assertEqual(len(self.calls), 1)
        self.assertFalse((self.bin / 'herdr-core').exists())
        self.assertFalse((self.bin / '.herdr-web-core-install.lock').exists())

    def test_failed_network_leaves_no_success_or_lock(self):
        with patch.object(launcher, 'core_download', side_effect=launcher.CoreBootstrapError('download failed')), self.assertRaisesRegex(launcher.CoreBootstrapError, 'download failed'):
            launcher.ensure_core(start=False)
        self.assertFalse((self.bin / 'herdr-core').exists())
        self.assertFalse((self.bin / '.herdr-web-core-install.lock').exists())

    def test_interruption_cleans_install_lock(self):
        with patch.object(launcher, 'core_download', side_effect=KeyboardInterrupt), self.assertRaises(KeyboardInterrupt):
            launcher.ensure_core(start=False)
        self.assertFalse((self.bin / '.herdr-web-core-install.lock').exists())

    def test_racing_or_partial_install_is_not_overwritten(self):
        (self.bin / 'herdr-core').symlink_to('/nonexistent')
        with patch.object(launcher, 'core_download', side_effect=AssertionError('download')), self.assertRaisesRegex(launcher.CoreBootstrapError, 'appeared'):
            launcher.ensure_core(start=False)

    def test_managed_update_hints(self):
        self.assertEqual(launcher.core_update_hint(Path('/opt/homebrew/Cellar/herdr-core/bin/hc')), 'brew upgrade herdr-core')
        self.assertIn('Nix', launcher.core_update_hint(Path('/nix/store/abc/bin/hc')))
        self.assertEqual(launcher.core_update_hint(Path('/home/test/.local/share/mise/installs/herdr-core/bin/hc')), 'mise upgrade herdr-core')

    def test_live_core_hello_reuses_no_binary(self):
        path, _ = launcher.core_socket_path()
        path.parent.mkdir(parents=True)
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(str(path))
            listener.listen()
            def serve():
                conn, _ = listener.accept()
                with conn:
                    request = json.loads(conn.recv(4096))
                    self.assertEqual(request['method'], 'hello')
                    conn.sendall(b'{"jsonrpc":"2.0","id":1,"result":{"generation":1,"framed":true}}\n')
            thread = threading.Thread(target=serve)
            thread.start()
            with patch.object(launcher, 'core_candidates', side_effect=AssertionError('discovery')):
                launcher.ensure_core()
            thread.join(2)

    def test_stopped_core_uses_start_not_restart(self):
        core = self.fake_core()
        real_run = launcher.subprocess.run
        seen = []
        def run(args, **kwargs):
            seen.append(args)
            if args[1:] == ['server', 'start', '--json']:
                return SimpleNamespace(returncode=0)
            return real_run(args, **kwargs)
        with patch.object(launcher, 'core_hello', side_effect=[False, True]), patch.object(launcher.subprocess, 'run', side_effect=run):
            launcher.ensure_core()
        self.assertEqual(seen[-1], [str(core), 'server', 'start', '--json'])


class ControlTests(unittest.TestCase):
    def setUp(self):
        self.asset = {'version': '1.0.0'}
        self.owner = SimpleNamespace(client=SimpleNamespace(base='', manifest=lambda wanted: {'version': '1.1.0'}), child_version='1.0.0')
        self.control = launcher.UpdateControl(self.owner, self.asset)
        self.addCleanup(self.control.close)

    def request(self, action, **kwargs):
        with socket.socket(socket.AF_UNIX) as conn:
            conn.settimeout(2)
            conn.connect(self.control.path)
            conn.sendall(json.dumps(dict(schemaVersion=1, action=action, **kwargs)).encode() + b'\n')
            return json.loads(conn.recv(16384))

    def test_initial_status_and_socket_permissions(self):
        self.assertEqual(self.request('status')['status'], 'idle')
        self.assertEqual(Path(self.control.path).stat().st_mode & 0o777, 0o600)
        self.assertEqual(Path(self.control.path).parent.stat().st_mode & 0o777, 0o700)
        self.assertEqual(self.request('shell')['status'], 'failed')

    def test_check_and_compare_and_swap(self):
        self.control.check()
        data = self.request('status')
        self.assertEqual(data['status'], 'available')
        self.assertEqual(data['availableVersion'], '1.1.0')
        self.assertEqual(self.request('update', expectedVersion='9.9.9')['status'], 'failed')
        finished = threading.Event()
        with patch.object(launcher, 'install', side_effect=lambda client, cache, asset: (finished.set(), Path('/verified'))[1]):
            self.owner.cache = Path('/unused')
            self.assertEqual(self.request('update', expectedVersion='1.1.0')['status'], 'updating')
            self.assertTrue(finished.wait(2))
            deadline = time.monotonic() + 2
            while self.control.pending is None and time.monotonic() < deadline:
                time.sleep(0.001)
        self.assertIsNotNone(self.control.pending)
        self.assertEqual(self.control.pending[1]['version'], '1.1.0')

    def test_status_responsive_and_duplicate_update_serialized(self):
        started, release = threading.Event(), threading.Event()
        def manifest(target):
            started.set()
            release.wait(2)
            return {'version': '1.1.0'}
        self.owner.client.manifest = manifest
        try:
            self.assertEqual(self.request('check')['status'], 'checking')
            self.assertTrue(started.wait(1))
            self.assertEqual(self.request('status')['status'], 'checking')
            self.assertEqual(self.request('check')['status'], 'checking')
        finally:
            release.set()

    def test_main_cas_requires_revision(self):
        self.owner.client.base = '/main'
        self.control.candidate = {'version': '1.0.0', 'revision': 'a' * 40, 'channel': 'main'}
        self.assertEqual(self.request('update', expectedVersion='1.0.0')['status'], 'failed')
        self.assertEqual(self.request('update', expectedVersion='1.0.0', expectedRevision='b' * 40)['status'], 'failed')


if __name__ == '__main__':
    unittest.main()
