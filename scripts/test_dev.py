"""Stdlib-only tests: python3 -B -m unittest discover -s scripts -p test_dev.py."""
import argparse
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location('herdr_dev', Path(__file__).with_name('dev.py'))
dev = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dev)


class DevTests(unittest.TestCase):
    def test_origins_preserved_exactly(self):
        for value in ['http://192.168.1.2:9001', 'http://localhost:9001', 'http://[::1]:9001', 'http://dev.example:9001']:
            self.assertEqual(dev.origin_value(value), value)

    def test_invalid_origins(self):
        for value in ['https://host:9001', 'http://host:9001/', 'http://host:9001/path',
                      'http://user@host:9001', 'http://user:pw@host:9001', 'http://host:9001?x',
                      'http://host:9001#x', 'http://host:9001?', 'http://host:9001#',
                      'http://host:0', 'http://host:99999', 'http://host:abc', '//host:9001',
                      'http://', 'http://host\n:9001', 'http://host\\:9001']:
            with self.subTest(value=value), self.assertRaises(argparse.ArgumentTypeError):
                dev.origin_value(value)

    def test_env_isolation_and_modes(self):
        inherited = {'PATH': '/usr/bin:/bin', 'HOME': '/live/home', 'HC_SOCKET': '/live/core.sock',
                     'HC_SESSION': 'live', 'HC_TERMINAL_ID': 'live-terminal', 'HC_FUTURE_SETTING': 'live',
                     'HERDR_WEB_PUBLIC_ORIGIN': 'http://evil', 'BASH_ENV': '/live/hook',
                     'NODE_OPTIONS': '--require=/live/hook', 'AWS_SECRET_ACCESS_KEY': 'secret', 'TERM': 'xterm'}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            env = dev.isolated_env(root, inherited)
            self.assertEqual({k for k in env if k.startswith('HC_')}, {'HC_SESSION', 'HC_SOCKET'})
            self.assertEqual(env['HC_SOCKET'], str(root / 'core.sock'))
            self.assertEqual(env['PATH'], inherited['PATH'])
            for key in ['BASH_ENV', 'NODE_OPTIONS', 'AWS_SECRET_ACCESS_KEY', 'HERDR_WEB_PUBLIC_ORIGIN']:
                self.assertNotIn(key, env)
            for key in ['HOME', 'XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR']:
                self.assertEqual(stat.S_IMODE(Path(env[key]).stat().st_mode), 0o700)
                self.assertTrue(Path(env[key]).is_relative_to(root))

    def test_private_files_exclusive_and_private(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'connection.json'
            self.assertEqual(dev.validate_state_file(path), path)
            dev.private_write(path, 'sensitive')
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            with self.assertRaises(FileExistsError):
                dev.private_write(path, 'overwrite')
            with self.assertRaises(RuntimeError):
                dev.validate_state_file(path)
            path.unlink()
            path.symlink_to(Path(folder) / 'absent')
            with self.assertRaises(RuntimeError):
                dev.validate_state_file(path)
            Path(folder).chmod(0o755)
            with self.assertRaises(RuntimeError):
                dev.validate_state_file(Path(folder) / 'other.json')

    def test_stop_only_owned_group(self):
        proc = Mock(pid=8675309)
        with patch.object(dev.os, 'killpg') as kill:
            dev.stop_group(proc)
        self.assertEqual([call.args[0] for call in kill.call_args_list], [proc.pid] * 3)
        self.assertEqual([call.args[1] for call in kill.call_args_list], [signal.SIGINT, signal.SIGTERM, signal.SIGKILL])

    def test_cleanup_isolated_core_and_root(self):
        args = argparse.Namespace(keep=False)
        session = dev.Dev(args, '/bin/hc', Path('/binary'), 'http://localhost:9001')
        root = session.root
        session.core_attempted = True
        session.hc_call = Mock()
        session.children = [Mock(), Mock()]
        with patch.object(dev, 'stop_group') as stop:
            session.close()
        self.assertEqual(stop.call_args_list[0].args[0], session.children[1])
        session.hc_call.assert_called_once_with('server', 'stop')
        self.assertFalse(root.exists())

    def test_cleanup_rejects_foreign_socket(self):
        session = dev.Dev(argparse.Namespace(keep=False), '/bin/hc', Path('/binary'), 'http://localhost:9001')
        session.core_attempted = True
        session.env['HC_SOCKET'] = '/live/core.sock'
        session.hc_call = Mock()
        session.close()
        session.hc_call.assert_not_called()

    def test_startup_failure_cleanup_does_not_touch_unowned_state(self):
        session = dev.Dev(argparse.Namespace(keep=False), '/bin/hc', Path('/binary'), 'http://localhost:9001')
        with tempfile.TemporaryDirectory() as folder:
            foreign = Path(folder) / 'connection.json'
            foreign.write_text('keep')
            session.args.state_file = foreign
            session.close()
            self.assertEqual(foreign.read_text(), 'keep')

    def test_readiness_fails_when_owned_child_exits(self):
        session = dev.Dev(argparse.Namespace(keep=False), '/bin/hc', Path('/binary'), 'http://localhost:9001')
        child = Mock()
        child.poll.return_value = 1
        session.children.append(child)
        try:
            with self.assertRaisesRegex(RuntimeError, 'exited during startup'):
                session.ready(9002, '/api/mode', 200, dev.time.monotonic() + 30)
        finally:
            with patch.object(dev, 'stop_group'):
                session.close()

    def test_start_uses_exact_origin_and_keeps_credentials_private(self):
        args = argparse.Namespace(keep=False, port=9001, backend_port=9002, state_file=None)
        origin = 'http://192.168.1.2:9001'
        session = dev.Dev(args, '/installed/hc', Path('/binary'), origin)
        session.hc_call = Mock(side_effect=['', json.dumps({'address': 't1'})])
        session.rpc = Mock()
        session.ready = Mock()
        backend, vite = Mock(pid=123), Mock(pid=456)
        backend.poll.return_value = vite.poll.return_value = None
        session.spawn = Mock(side_effect=[backend, vite])
        output = io.StringIO()
        try:
            with patch.object(dev.time, 'sleep', side_effect=KeyboardInterrupt()), \
                    contextlib.redirect_stdout(output):
                with self.assertRaises(KeyboardInterrupt):
                    session.start()
            state = json.loads((session.root / 'connection.json').read_text())
            token = (session.root / 'token').read_text()
            self.assertNotIn(token, output.getvalue())
            self.assertNotIn('#token=', output.getvalue())
            self.assertEqual(state['baseOrigin'], origin)
            self.assertEqual(state['privateSignInUrl'], origin + '/#token=' + token)
            self.assertEqual(state['childPIDs'], [123, 456])
            argv = session.spawn.call_args_list[0].args[0]
            self.assertEqual(argv[argv.index('--public-origin') + 1], origin)
            self.assertEqual(argv[argv.index('--bind') + 1], '127.0.0.1')
            vite_env = session.spawn.call_args_list[1].kwargs['env']
            self.assertEqual(vite_env['HERDR_DEV_ORIGIN'], origin)
            self.assertEqual(vite_env['HERDR_API_URL'], 'http://127.0.0.1:9002')
            self.assertEqual(session.rpc.call_args.args[0], 'doc.set')
        finally:
            session.core_attempted = False
            session.close()

    def test_real_owned_child_group_cleanup(self):
        child = subprocess.Popen([sys.executable, '-B', '-c', 'import time; time.sleep(60)'],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        try:
            dev.stop_group(child)
            self.assertIsNotNone(child.poll())
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()

    def test_existing_binary_never_invokes_make(self):
        with tempfile.TemporaryDirectory() as folder:
            binary = Path(folder) / 'binary'
            binary.write_text('#!/bin/sh\n')
            binary.chmod(0o700)
            fake = Mock()
            fake.start.side_effect = KeyboardInterrupt()
            with patch.object(sys, 'argv', ['dev.py', '--origin', 'http://localhost:9001', '--binary', str(binary)]), \
                    patch.object(dev.shutil, 'which', return_value='/usr/bin/hc'), \
                    patch.object(dev, 'check_ports'), patch.object(dev.Path, 'is_file', return_value=True), \
                    patch.object(dev, 'Dev', return_value=fake), patch.object(dev.subprocess, 'run') as run:
                self.assertEqual(dev.main(), 0)
            run.assert_not_called()
            fake.close.assert_called_once()

    def test_missing_binary_bootstraps_once(self):
        with tempfile.TemporaryDirectory() as folder:
            binary = Path(folder) / 'binary'
            fake = Mock()
            fake.start.side_effect = KeyboardInterrupt()
            with patch.object(sys, 'argv', ['dev.py', '--origin', 'http://localhost:9001', '--binary', str(binary)]), \
                    patch.object(dev.shutil, 'which', return_value='/usr/bin/hc'), \
                    patch.object(dev, 'check_ports'), patch.object(dev.Path, 'is_file', return_value=True), \
                    patch.object(dev.os, 'access', return_value=True), patch.object(dev, 'Dev', return_value=fake), \
                    patch.object(dev.subprocess, 'run') as run:
                self.assertEqual(dev.main(), 0)
            run.assert_called_once_with(['make', 'dev-build'], cwd=dev.PROJECT, check=True)

    def test_no_build_missing_binary_fails_without_make(self):
        with tempfile.TemporaryDirectory() as folder:
            with patch.object(sys, 'argv', ['dev.py', '--no-build', '--origin', 'http://localhost:9001', '--binary', folder + '/absent']), \
                    patch.object(dev.shutil, 'which', return_value='/usr/bin/hc'), \
                    patch.object(dev, 'check_ports'), patch.object(dev.subprocess, 'run') as run, \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(dev.main(), 1)
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
