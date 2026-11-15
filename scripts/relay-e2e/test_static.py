"""Offline checks only: no Core, browser, server, or containers are started."""
import ast
import hashlib
import hmac
import importlib.util
import os
from pathlib import Path
import socket
import ssl
import struct
import tempfile
import types
import unittest
from unittest.mock import patch, Mock

SPEC = importlib.util.spec_from_file_location('relay_e2e', Path(__file__).parent.parent / 'relay-e2e.py')
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


def attr(kind, value):
    return struct.pack('!HH', kind, len(value)) + value + b'\0' * (-len(value) % 4)


class TurnSocket:
    """In-memory RFC5389 challenge/auth exchange; never opens a socket."""
    def __init__(self, protocol, bad_realm=False):
        self.protocol = protocol
        self.bad_realm = bad_realm
        self.response = b''
        self.requests = []

    def __enter__(self):
        return self

    def __exit__(self, *_):
        pass

    def settimeout(self, _):
        pass

    def connect(self, address):
        assert address == ('127.0.0.1', 4567)

    def sendall(self, packet):
        self.requests.append(packet)
        assert packet[:2] == b'\0\x03'
        assert struct.unpack('!H', packet[2:4])[0] == len(packet) - 20
        if len(self.requests) == 1:
            attributes = attr(9, b'\0\0\x04\x01') + attr(0x14, b'wrong' if self.bad_realm else b'e2e') + attr(0x15, b'nonce')
            kind = 0x113
        else:
            attrs = {}
            pos = 20
            while pos < len(packet):
                kind, size = struct.unpack('!HH', packet[pos:pos + 4])
                attrs[kind] = packet[pos + 4:pos + 4 + size]
                pos += 4 + size + (-size % 4)
            import base64
            password = base64.b64encode(hmac.new(b'secret', attrs[6], hashlib.sha1).digest())
            key = hashlib.md5(attrs[6] + b':e2e:' + password).digest()
            assert attrs[8] == hmac.new(key, packet[:-24], hashlib.sha1).digest()
            assert attrs[0x15] == b'nonce'
            attributes, kind = b'', 0x103
        self.response = struct.pack('!HH', kind, len(attributes)) + packet[4:20] + attributes

    def recv(self, size):
        # Force partial TCP reads to exercise framing, including the header.
        size = min(size, 7) if self.protocol in ('tcp', 'tls') else size
        result, self.response = self.response[:size], self.response[size:]
        return result


class OfflineTests(unittest.TestCase):
    def browser_helper(self):
        # Execute only the class: no CDP connection, optional GUI packages or I/O.
        source = (Path(__file__).parent / 'browser.py').read_text()
        nodes = [n for n in ast.parse(source).body if isinstance(n, ast.ClassDef) and n.name in ('Browser', 'NavigationPending')]
        namespace = {'json': __import__('json'), 'time': __import__('time')}
        exec(compile(ast.Module(body=nodes, type_ignores=[]), '<browser-helper>', 'exec'), namespace)
        browser = object.__new__(namespace['Browser'])
        browser.js = Mock(return_value={'x': 10, 'y': 20})
        browser.call = Mock()
        browser.wait = Mock()
        return browser

    def test_wait_retries_navigation_but_not_real_page_errors(self):
        browser = self.browser_helper()
        wait = type(browser).wait.__get__(browser)
        pending = type(browser).call.__globals__['NavigationPending']
        browser.js.side_effect = [pending('Browser navigation pending'), True]
        wait('true', timeout=1)
        self.assertEqual(browser.js.call_count, 2)
        browser.js.side_effect = RuntimeError('page evaluation failed')
        with self.assertRaisesRegex(RuntimeError, 'page evaluation failed'):
            wait('bad expression', timeout=1)

    def test_reload_fences_old_document_before_waiting(self):
        browser = self.browser_helper()
        calls = Mock()
        calls.attach_mock(browser.js, 'js')
        calls.attach_mock(browser.call, 'call')
        calls.attach_mock(browser.wait, 'wait')
        browser.reload("!!document.querySelector('main')", timeout=8)
        self.assertEqual([c[0] for c in calls.mock_calls], ['js', 'call', 'wait'])
        browser.js.assert_called_once_with('window.__herdrReloadFence=true')
        self.assertIn('!window.__herdrReloadFence', browser.wait.call_args.args[0])
        self.assertEqual(browser.wait.call_args.kwargs['timeout'], 8)

    def test_disconnect_handles_local_pane_and_relay_teardown(self):
        browser = self.browser_helper()
        browser.disconnected()
        expression = browser.wait.call_args.args[0]
        self.assertIn("!document.querySelector('.xterm')", expression)
        self.assertIn("document.querySelector('.terminal-detail')", expression)

    def test_click_waits_then_sends_real_pointer_events(self):
        browser = self.browser_helper()
        browser.click('button[aria-label="Settings"]')
        self.assertIn('!e.disabled', browser.wait.call_args.args[0])
        self.assertEqual([c.args[1]['type'] for c in browser.call.call_args_list],
                         ['mousePressed', 'mouseReleased'])

    def test_sources_compile_without_bytecode(self):
        directory = Path(__file__).parent
        for source in [directory.parent / 'relay-e2e.py', *directory.glob('*.py')]:
            compile(source.read_bytes(), str(source), 'exec')

    def test_turn_auth_udp_and_tcp(self):
        for protocol in ('udp', 'tcp'):
            fake = TurnSocket(protocol)
            with patch.object(socket, 'socket', return_value=fake):
                runner.turn_auth(4567, protocol, 'secret', 'e2e')
            self.assertEqual(len(fake.requests), 2)

    def test_tls_probe_verified_context_and_stream_framing(self):
        fake = TurnSocket('tls')
        context = Mock()
        context.wrap_socket.return_value = fake
        with patch.object(socket, 'socket', return_value=fake), \
             patch.object(ssl, 'create_default_context', return_value=context) as create:
            runner.turn_auth(4567, 'tls', 'secret', 'e2e', Path('/private/ca.pem'))
        create.assert_called_once_with(cafile='/private/ca.pem')
        context.wrap_socket.assert_called_once_with(fake, server_hostname='127.0.0.1')
        self.assertEqual(len(fake.requests), 2)

    def test_tls_requires_ca_and_never_falls_back(self):
        fake = TurnSocket('tls')
        with patch.object(socket, 'socket', return_value=fake), self.assertRaises(runner.Failure):
            runner.turn_auth(4567, 'tls', 'secret', 'e2e')
        self.assertFalse(fake.requests)
        context = Mock()
        context.wrap_socket.side_effect = ssl.SSLCertVerificationError('private diagnostic')
        with patch.object(socket, 'socket', return_value=fake) as make_socket, \
             patch.object(ssl, 'create_default_context', return_value=context), \
             self.assertRaises(ssl.SSLCertVerificationError):
            runner.turn_auth(4567, 'tls', 'secret', 'e2e', '/private/ca.pem')
        self.assertEqual(make_socket.call_count, 1)
        self.assertFalse(fake.requests)

    def test_reload_observer_registered_before_navigation(self):
        source = (Path(__file__).parent / 'browser.py').read_text()
        reload_block = source.split("elif mode == 'reload':", 1)[1].split("elif mode == 'resume':", 1)[0]
        self.assertLess(reload_block.index('Page.addScriptToEvaluateOnNewDocument'),
                        reload_block.index('Page.reload'))
        self.assertIn("label='reload observer installed'", reload_block)

    def test_tls_certificate_ip_san_and_private_mount_material(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            harness = object.__new__(runner.Harness)
            ca = harness.make_tls(root)
            runner.run(['openssl', 'verify', '-CAfile', str(ca), '-verify_ip', '127.0.0.1',
                        str(root / 'leaf.pem')])
            with self.assertRaises(runner.Failure):
                runner.run(['openssl', 'verify', '-CAfile', str(ca), '-verify_ip', '127.0.0.2',
                            str(root / 'leaf.pem')])
            self.assertEqual({p.name for p in root.iterdir()}, {'ca.pem', 'leaf.pem', 'leaf.key'})
            self.assertTrue(all(p.stat().st_mode & 0o777 == 0o600 for p in root.iterdir()))

    def test_tls_signing_without_serial_artifact(self):
        real_run = runner.run
        def without_serial(args, **kwargs):
            result = real_run(args, **kwargs)
            if '-CAserial' in args:
                Path(args[args.index('-CAserial') + 1]).unlink(missing_ok=True)
            return result
        with tempfile.TemporaryDirectory() as directory, patch.object(runner, 'run', side_effect=without_serial):
            root = Path(directory)
            object.__new__(runner.Harness).make_tls(root)
            self.assertEqual({p.name for p in root.iterdir()}, {'ca.pem', 'leaf.pem', 'leaf.key'})

    def test_external_mode_keeps_local_services_inside_local_branch(self):
        tree = ast.parse((Path(__file__).parent.parent / 'relay-e2e.py').read_text())
        harness = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == 'Harness')
        execute = next(n for n in harness.body if isinstance(n, ast.FunctionDef) and n.name == 'execute')
        local = next(n for n in execute.body if isinstance(n, ast.If) and ast.unparse(n.test) == 'not external')
        def starts(node):
            return [n for n in ast.walk(node) if isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute)
                    and (n.func.attr == 'start_turn' or (n.func.attr == 'spawn' and n.args
                         and isinstance(n.args[-1], ast.Constant) and n.args[-1].value == 'relay'))]
        self.assertEqual(len(starts(execute)), 2)
        self.assertEqual(len(starts(local)), 2)
        flags = next(n for n in execute.body if isinstance(n, ast.Assign)
                     and any(isinstance(t, ast.Name) and t.id == 'browser_flags' for t in n.targets))
        self.assertIsInstance(flags.value, ast.IfExp)
        self.assertIn('not external', ast.unparse(flags.value.test))

    def test_browser_rejects_mixed_turn_transports(self):
        source = (Path(__file__).parent / 'browser.py').read_text()
        self.assertIn("value['turnTransports'] == [transport]", source)
        self.assertIn('p.getConfiguration().iceServers', source)

    def test_turn_bad_realm_fails_closed(self):
        fake = TurnSocket('udp', bad_realm=True)
        with patch.object(socket, 'socket', return_value=fake), self.assertRaises(runner.Failure):
            runner.turn_auth(4567, 'udp', 'secret', 'e2e')
        self.assertEqual(len(fake.requests), 1)

    def test_environment_isolation_and_cleanup(self):
        with patch.dict(os.environ, {'HC_SOCKET': '/never/user.sock', 'HC_SESSION': 'user',
                                      'HERDR_RELAY_URL': 'private', 'AWS_SECRET_ACCESS_KEY': 'private'}), \
             patch.object(runner.shutil, 'which', return_value='/fake/hc'):
            harness = runner.Harness(types.SimpleNamespace())
        root = harness.root
        try:
            self.assertEqual(root.stat().st_mode & 0o777, 0o700)
            self.assertTrue(Path(harness.env['HC_SOCKET']).is_relative_to(root))
            self.assertNotIn('HERDR_RELAY_URL', harness.env)
            self.assertNotIn('AWS_SECRET_ACCESS_KEY', harness.env)
            for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_RUNTIME_DIR'):
                self.assertTrue(Path(harness.env[key]).is_relative_to(root))
        finally:
            harness.close()
        self.assertFalse(root.exists())

    def test_private_files_no_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'secret'
            runner.private(path, 'example')
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                runner.private(path, 'replacement')


if __name__ == '__main__':
    unittest.main()
