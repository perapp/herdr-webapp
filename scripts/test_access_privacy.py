#!/usr/bin/env python3
"""Owned-PTY access-display acceptance; never reads or changes the caller's TTY."""
import argparse
import fcntl
import http.cookiejar
import json
import os
from pathlib import Path
import re
import secrets
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time
import urllib.error
import urllib.request


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    binary = parser.parse_args().binary.resolve()
    with tempfile.TemporaryDirectory(prefix='herdr-access-privacy-') as temporary:
        root = Path(temporary)
        token = secrets.token_urlsafe(32)
        token_file = root / 'token'
        token_file.write_text(token)
        token_file.chmod(0o600)
        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 50, 160, 0, 0))

        def controlling_tty():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        env = os.environ | {'HOME': temporary, 'XDG_STATE_HOME': temporary,
                            'TERM': 'xterm-256color', 'HC_SOCKET': str(root / 'absent.sock')}
        process = None
        try:
            process = subprocess.Popen([str(binary), '--bind', '127.0.0.1', '--port', '0',
                '--core-socket', str(root / 'absent.sock'), '--token-file', str(token_file)],
                stdin=slave, stdout=slave, stderr=slave, cwd=root, env=env,
                preexec_fn=controlling_tty, close_fds=True)
            os.close(slave)
            slave = None

            def output_until(predicate):
                output = bytearray()
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    assert process.poll() is None, 'owned display process exited'
                    ready, _, _ = select.select([master], [], [], max(0, deadline - time.monotonic()))
                    if ready:
                        output.extend(os.read(master, 65536))
                        assert len(output) <= 1 << 20, 'owned display exceeded output bound'
                        if predicate(bytes(output)):
                            return bytes(output)
                raise AssertionError('owned display state did not arrive')

            initial = output_until(lambda data: b'#token=' + token.encode() in data)
            match = re.search(rb'http://127\.0\.0\.1:(\d+)', initial)
            assert match is not None, 'owned display origin missing'
            origin = 'http://127.0.0.1:' + match.group(1).decode()
            first = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
            other = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

            def request(client, path, body=None, headers=None):
                data = json.dumps(body).encode() if body is not None else None
                req = urllib.request.Request(origin + path, data=data,
                    headers={'Origin': origin, 'Content-Type': 'application/json'} | (headers or {}))
                try:
                    with client.open(req, timeout=10) as response:
                        return response.status, json.load(response)
                except urllib.error.HTTPError as error:
                    return error.code, None

            assert request(first, '/api/login', {'token': token})[0] == 200
            output_until(lambda data: b'Private URL and QR hidden.' in data)
            # Encoded keys/protocol replies must not toggle; one intentional L does.
            os.write(master, b'\x1b[1;2L\x1b]10;lll\x07L')
            output_until(lambda data: b'#token=' + token.encode() in data)
            assert request(other, '/api/login', {'token': token})[0] == 200
            # A later authenticated profile must not undo the explicit reveal.
            os.write(master, b'l')
            output_until(lambda data: b'Private URL and QR hidden.' in data)
            assert request(first, '/api/access/rotate', {}) == (200, {'rotated': True})
            output_until(lambda data: b'Private URL and QR hidden.' in data)
            rotated = token_file.read_text().strip()
            assert rotated != token, 'owned access file did not rotate'
            os.write(master, b'L')
            revealed = output_until(lambda data: b'#token=' + rotated.encode() in data)
            assert token.encode() not in revealed, 'obsolete private link reappeared'
            assert request(first, '/api/session')[0] == 200
            assert request(other, '/api/session')[0] == 401
            anonymous = urllib.request.build_opener()
            assert request(anonymous, '/api/session', headers={'Authorization': 'Bearer ' + token})[0] == 401
            print('PASS native first-auth hide, L/encoded-key privacy, hidden rotation, new-link reveal and retained browser')
        finally:
            if process is not None:
                if process.poll() is None:
                    process.send_signal(signal.SIGINT)
                    try:
                        # Drain the owned PTY while Dashboard restores its screen.
                        deadline = time.monotonic() + 5
                        while process.poll() is None and time.monotonic() < deadline:
                            if select.select([master], [], [], .1)[0]:
                                try:
                                    os.read(master, 65536)
                                except OSError:
                                    break
                        process.wait(timeout=max(.1, deadline - time.monotonic()))
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
            os.close(master)
            if slave is not None:
                os.close(slave)


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # No PTY content, token, URL, cookie or exception text crosses the boundary.
        print('FAIL owned access-display acceptance: ' + type(error).__name__)
        raise SystemExit(1) from None
