#!/usr/bin/env python3
"""Isolated Core regression: automatic bytes must never masquerade as human drafts.

Paired with terminalInput.test.ts (real xterm deferred-human source -> DATA),
this proves why preserving that source prevents a queued agent message from
appending to and submitting an untracked phone draft. Never uses the live Core.
"""
import json
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest


@unittest.skipUnless(shutil.which('hc'), 'installed hc required for isolated Core regression')
class ProvenanceTests(unittest.TestCase):
    def test_delayed_human_draft_defers_agent_message_but_automatic_input_does_not(self):
        with tempfile.TemporaryDirectory(prefix='herdr-provenance-') as directory:
            root = Path(directory)
            env = {k: os.environ[k] for k in ('PATH', 'LANG', 'USER', 'LOGNAME', 'SHELL') if k in os.environ}
            for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_RUNTIME_DIR', 'XDG_DATA_HOME'):
                folder = root / key
                folder.mkdir(mode=0o700)
                env[key] = str(folder)
            env.update(HC_SOCKET=str(root / 'core.sock'), HC_SESSION='provenance',
                       HC_QUIET_MS='20', HC_DRAFT_MS='60000', HC_STARTED_MS='50')
            hc = shutil.which('hc')
            def command(*args):
                result = subprocess.run([hc, *args], env=env, cwd=root, capture_output=True, timeout=15)
                self.assertEqual(result.returncode, 0, 'isolated Core command failed (private diagnostics suppressed)')
                return result.stdout
            command('server', 'start')
            try:
                for human in (False, True):
                    with self.subTest(human=human):
                        log = root / ('human' if human else 'automatic')
                        ready = root / (log.name + '-ready')
                        program = ("import os,tty;from pathlib import Path;tty.setraw(0)\n"
                                   f"Path({str(ready)!r}).touch();f=open({str(log)!r},'ab',buffering=0)\n"
                                   "while True:\n f.write(os.read(0,4096))\n")
                        address = json.loads(command('run', '--bg', '--name', log.name,
                                                     sys.executable, '-c', program))['address']
                        self.until(lambda: ready.exists(), 'raw editor started')
                        viewer = socket.socket(socket.AF_UNIX)
                        viewer.settimeout(5)
                        viewer.connect(env['HC_SOCKET'])
                        stream = viewer.makefile('rb')
                        viewer.sendall(b'{"jsonrpc":"2.0","id":0,"method":"hello","params":{"generation":1,"framed":true}}\n')
                        self.assertNotIn('error', json.loads(stream.readline()))
                        def frame(channel, kind, data):
                            viewer.sendall(struct.pack('<IBI', channel, kind, len(data)) + data)
                        request = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': 'terminal.attach',
                                              'params': {'terminal': address, 'cols': 80, 'rows': 24}}).encode()
                        frame(0, 0, request)
                        channel = None
                        while channel is None:
                            header = stream.read(9)
                            self.assertEqual(len(header), 9)
                            ch, kind, length = struct.unpack('<IBI', header)
                            value = stream.read(length)
                            if ch == 0 and kind == 0:
                                reply = json.loads(value)
                                if reply.get('id') == 1:
                                    self.assertNotIn('error', reply)
                                    channel = reply['result']['channel']
                        frame(channel, 0 if human else 6, b'PHONE_DRAFT')
                        self.until(lambda: log.exists() and log.read_bytes() == b'PHONE_DRAFT', 'draft reached editor')
                        finished = threading.Event()
                        errors = []
                        def send_notice():
                            try:
                                with socket.socket(socket.AF_UNIX) as caller:
                                    caller.settimeout(8)
                                    caller.connect(env['HC_SOCKET'])
                                    with caller.makefile('rwb') as rpc:
                                        for ident, method, params in [(0, 'hello', {'generation': 1}),
                                                (1, 'terminal.send', {'terminal': address, 'text': 'AGENT_NOTICE'})]:
                                            rpc.write((json.dumps({'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params}) + '\n').encode())
                                            rpc.flush()
                                            result = json.loads(rpc.readline())
                                            if 'error' in result:
                                                errors.append('isolated send failed')
                            except Exception as error:
                                errors.append(type(error).__name__)
                            finally:
                                finished.set()
                        thread = threading.Thread(target=send_notice, daemon=True)
                        thread.start()
                        try:
                            if human:
                                self.assertFalse(finished.wait(.3), 'agent send bypassed the tracked human draft')
                                self.assertEqual(log.read_bytes(), b'PHONE_DRAFT', 'agent notice appended/submitted human draft')
                                frame(channel, 0, b'\r')  # Only an explicit human submit releases the queued notice.
                            self.assertTrue(finished.wait(5), 'isolated agent notice did not finish')
                            self.assertFalse(errors, str(errors))
                            self.until(lambda: b'AGENT_NOTICE' in log.read_bytes() and b'\r' in log.read_bytes(), 'notice delivered')
                            if human:
                                self.assertTrue(log.read_bytes().startswith(b'PHONE_DRAFT\r'))
                            else:
                                data = log.read_bytes()
                                self.assertTrue(data.startswith(b'PHONE_DRAFT'))
                                self.assertNotIn(b'\r', data[:data.index(b'AGENT_NOTICE')])
                        finally:
                            stream.close()
                            viewer.close()
                            thread.join(timeout=1)
            finally:
                command('server', 'stop')

    def until(self, predicate, label):
        deadline = time.monotonic() + 5
        while not predicate():
            self.assertLess(time.monotonic(), deadline, label)
            time.sleep(.01)


if __name__ == '__main__':
    unittest.main()
