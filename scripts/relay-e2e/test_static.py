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
from unittest.mock import patch, Mock, MagicMock

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
        browser.wait = Mock(return_value={'x': 10, 'y': 20})
        return browser

    def test_wait_retries_navigation_but_not_real_page_errors(self):
        browser = self.browser_helper()
        wait = type(browser).wait.__get__(browser)
        pending = type(browser).call.__globals__['NavigationPending']
        browser.js.side_effect = [pending('Browser navigation pending'), True]
        self.assertIs(wait('true', timeout=1), True)
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
        self.assertIn('document.hasFocus()', browser.wait.call_args_list[0].args[0])
        self.assertIn("document.visibilityState==='visible'", browser.wait.call_args_list[0].args[0])
        expression = browser.wait.call_args.args[0]
        self.assertIn('e.disabled', expression)
        self.assertIn('e?.isConnected', expression)
        self.assertIn('document.elementFromPoint', expression)
        self.assertNotIn('requestAnimationFrame', expression)
        browser.js.assert_not_called()
        self.assertEqual([c.args[0] for c in browser.call.call_args_list],
                         ['Page.bringToFront', 'Input.dispatchMouseEvent', 'Input.dispatchMouseEvent'])
        self.assertEqual([c.args[1]['type'] for c in browser.call.call_args_list[1:]],
                         ['mousePressed', 'mouseReleased'])

    def test_click_never_dispatches_before_actual_page_focus(self):
        browser = self.browser_helper()
        browser.wait.side_effect = AssertionError('input page focused')
        with self.assertRaisesRegex(AssertionError, 'input page focused'):
            browser.click('.terminal-host')
        browser.call.assert_called_once_with('Page.bringToFront')

    def test_click_does_not_replay_after_pointer_dispatch_failure(self):
        browser = self.browser_helper()
        browser.call.side_effect = [{}, {}, RuntimeError('CDP operation failed')]
        with self.assertRaisesRegex(RuntimeError, 'CDP operation failed'):
            browser.click('button')
        self.assertEqual([c.args[0] for c in browser.call.call_args_list],
                         ['Page.bringToFront', 'Input.dispatchMouseEvent', 'Input.dispatchMouseEvent'])
        self.assertEqual(browser.wait.call_count, 2)

    def test_nested_raw_draft_fixture_compiles_and_completion_is_one_shot(self):
        source = (Path(__file__).parent / 'browser.py').read_text()
        tree = ast.parse(source)
        code = next(n.value.func.value.value for n in ast.walk(tree)
                    if isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id == 'code' for t in n.targets))
        compile(code.replace('PROOF', repr('/isolated-fixture-proof')), '<raw-draft-fixture>', 'exec')
        # OSC color replies end in BEL too; they must not retrigger the Ctrl-G paint.
        self.assertIn(f"if data=={bytes([7])!r} and not painted:", code)

    def profile_helper(self, fail=None):
        source = (Path(__file__).parent / 'browser.py').read_text()
        node = next(n for n in ast.parse(source).body if isinstance(n, ast.ClassDef) and n.name == 'BrowserProfile')
        calls, pages, sockets = [], [], []
        class FakeBrowser:
            def __init__(self, target):
                if fail == 'page':
                    raise RuntimeError('private page failure')
                self.ws = Mock()
                pages.append(self)
            def call(self, method, params=None):
                calls.append((method, params))
                if method == 'Target.createBrowserContext':
                    return {'browserContextId': 'owned-context-' + str(len(sockets))}
                if method == 'Target.createTarget':
                    if fail == 'target':
                        raise RuntimeError('private target failure')
                    return {'targetId': 'owned-page'}
                if method == 'Page.addScriptToEvaluateOnNewDocument' and fail == 'observer':
                    raise RuntimeError('private observer failure')
                if method == 'Target.disposeBrowserContext' and fail == 'dispose':
                    raise RuntimeError('private disposal failure')
                return {}
        def connect(*args, **kwargs):
            ws = Mock()
            sockets.append(ws)
            return ws
        namespace = {'Browser': FakeBrowser, 'OBSERVER': 'passive observer',
                     'urllib': types.SimpleNamespace(request=types.SimpleNamespace(urlopen=MagicMock())),
                     'json': types.SimpleNamespace(load=Mock(return_value={'webSocketDebuggerUrl': 'private endpoint'})),
                     'websocket': types.SimpleNamespace(create_connection=connect)}
        exec(compile(ast.Module(body=[node], type_ignores=[]), '<browser-profile>', 'exec'), namespace)
        return namespace['BrowserProfile'], calls, pages, sockets

    def test_profiles_own_distinct_contexts_and_dispose_on_body_failure(self):
        profile, calls, pages, sockets = self.profile_helper()
        with self.assertRaisesRegex(AssertionError, 'body failed'):
            with profile(), profile():
                raise AssertionError('body failed')
        created = [p['browserContextId'] for m, p in calls if m == 'Target.createTarget']
        disposed = [p['browserContextId'] for m, p in calls if m == 'Target.disposeBrowserContext']
        self.assertEqual(len(set(created)), 2)
        self.assertEqual(disposed, list(reversed(created)))
        self.assertTrue(all(p.ws.close.call_count == 1 for p in pages))
        self.assertTrue(all(s.close.call_count == 1 for s in sockets))
        self.assertFalse(any(m in ('Target.closeTarget', 'Target.activateTarget') for m, _ in calls))

    def test_profile_partial_setup_and_disposal_failures_close_sockets(self):
        for fail in ('target', 'page', 'observer', 'dispose'):
            with self.subTest(fail=fail):
                profile, calls, pages, sockets = self.profile_helper(fail)
                with self.assertRaises(RuntimeError):
                    with profile():
                        pass
                self.assertEqual([p for m, p in calls if m == 'Target.disposeBrowserContext'],
                                 [{'browserContextId': 'owned-context-1'}])
                self.assertTrue(all(p.ws.close.call_count == 1 for p in pages))
                self.assertEqual(sockets[0].close.call_count, 1)

    def test_rotation_uses_real_ui_original_capabilities_and_native_proofs(self):
        source = (Path(__file__).parent / 'browser.py').read_text()
        tree = ast.parse(source)
        rotation = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == 'rotation_acceptance')
        text = ast.get_source_segment(source, rotation)
        self.assertEqual(text.count("keeper.click('[aria-label=\"Confirm access token rotation\"] .danger')"), 1)
        self.assertEqual(text.count("job['pairing']"), 3)
        self.assertIn("job['secondPairing']", text)
        self.assertIn('rotationPeer.connectionState', text)
        self.assertIn('peers.length===window.__e2e.rotationCount', text)
        self.assertIn('denied_profile(other)', text)
        self.assertIn('denied_profile(fresh)', text)
        self.assertIn('rotation-second-existing-proof', text)
        self.assertIn('rotation-second-pairing-proof', text)
        settings = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == 'rotation_settings')
        settings_text = ast.get_source_segment(source, settings)
        self.assertIn("'width': 390", settings_text)
        self.assertIn("screenshot.chmod(0o600)", settings_text)
        self.assertIn('rotation_settings(keeper, False)', text)
        self.assertIn('rotation_settings(fresh_second, True)', text)
        self.assertIn("keeper.click('[aria-label=\"Close navigation\"]')", text)
        self.assertNotIn('fetch(', text)
        self.assertNotIn('/api/', text)
        runner_source = (Path(__file__).parent.parent / 'relay-e2e.py').read_text()
        self.assertLess(runner_source.index("self.browser('multi-computer')"), runner_source.index("self.browser('rotation')"))
        self.assertLess(runner_source.index("self.browser('rotation')"), runner_source.index("self.browser('large')"))

    def test_focused_rotation_is_explicit_and_never_claims_full_acceptance(self):
        source = (Path(__file__).parent.parent / 'relay-e2e.py').read_text()
        self.assertIn("'--focus-rotation'", source)
        tree = ast.parse(source)
        execute = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == 'execute')
        focused = next(n for n in execute.body if isinstance(n, ast.If) and ast.unparse(n.test) == 'self.args.focus_rotation')
        self.assertIsInstance(focused.body[-1], ast.Return)
        self.assertIn('focused-rotation', ast.unparse(focused))
        self.assertLess(source.index("self.browser('rotation')"), source.index("'scope': 'focused-rotation'"))
        self.assertLess(source.index("'scope': 'focused-rotation'"), source.index("self.browser('large')"))

    def test_private_binary_artifacts_are_owner_only_and_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'capture.png'
            runner.private(path, b'private image')
            self.assertEqual(path.read_bytes(), b'private image')
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                runner.private(path, b'other')

    def test_denial_waits_for_real_auth_retries_not_sleep(self):
        source = (Path(__file__).parent / 'browser.py').read_text()
        node = next(n for n in ast.parse(source).body if isinstance(n, ast.FunctionDef) and n.name == 'denied_profile')
        namespace = {}
        exec(compile(ast.Module(body=[node], type_ignores=[]), '<denied-profile>', 'exec'), namespace)
        browser = Mock()
        browser.js.return_value = True
        namespace['denied_profile'](browser)
        self.assertIn('authAttempts>=2', browser.wait.call_args.args[0])
        self.assertIn('Connection interrupted', browser.wait.call_args.args[0])
        self.assertIn('peers.length===0', browser.js.call_args.args[0])
        browser.js.return_value = False
        with self.assertRaisesRegex(AssertionError, 'rotation authorization rejected'):
            namespace['denied_profile'](browser)

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
