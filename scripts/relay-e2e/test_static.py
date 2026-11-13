"""Offline checks only: no Core, browser, server, or containers are started."""
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
