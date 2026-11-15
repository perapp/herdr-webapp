#!/usr/bin/env python3
"""Offline launcher tests: isolated loopback HTTP relay and real connector children."""
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

SCRIPT = Path(__file__).with_name('relay-launcher.py')
spec = importlib.util.spec_from_file_location('launcher', SCRIPT)
launcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launcher)


def eventually(predicate, timeout=5):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        if predicate():
            return
        time.sleep(0.025)
    raise AssertionError('condition did not become true')


class LauncherTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='herdr-relay-tests-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.events = self.root / 'events'
        self.assets = {}
        self.manifest = None
        self.hold = threading.Event()
        self.hold.set()
        self.downloading = threading.Event()
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path in ('/client/manifest.json', '/main/client/manifest.json'):
                    body = json.dumps(owner.manifest).encode()
                elif self.path == '/redirect':
                    self.send_response(302)
                    self.send_header('Location', 'http://example.invalid/escape')
                    self.end_headers()
                    return
                else:
                    body = owner.assets.get(self.path)
                    if body is None:
                        self.send_error(404)
                        return
                    owner.downloading.set()
                    owner.hold.wait(5)
                self.send_response(200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.url = 'http://127.0.0.1:' + str(self.server.server_port)
        self.children = []
        self.addCleanup(self.cleanup_children)
        self.publish('1.0.0')

    def cleanup_children(self):
        self.hold.set()
        for child in self.children:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            child.stdout.close()
            child.stderr.close()

    def publish(self, release, behavior='good', revision=None):
        body = ('''#!{python}
import json, os, pathlib, signal, sys, time
release = {release!r}
behavior = {behavior!r}
revision = {revision!r}
if sys.argv[1:] == ['--build-revision']:
    print(revision if behavior != 'revision' else '0' * 40)
    sys.exit(0)
if sys.argv[1:] in (['--version'], ['--package-version']):
    print('herdr-webapp ' + ('260603140506+0200-' + revision[:7] if revision and sys.argv[1] == '--version' else release))
    sys.exit(0)
events = pathlib.Path({events!r})
def event(kind):
    with events.open('a') as f:
        f.write(kind + ' ' + release + ' ' + str(os.getpid()) + '\\n')
def finish(sig, frame):
    event('stop')
    sys.exit(0)
signal.signal(signal.SIGTERM, finish)
signal.signal(signal.SIGINT, finish)
event('start')
events.with_suffix('.control').write_text(os.environ.get('HERDR_CONNECTOR_CONTROL_SOCKET', ''))
state = pathlib.Path(sys.argv[sys.argv.index('--state-dir') + 1])
pair = state / 'pairing-test'
if not pair.exists():
    pair.write_text('preserve pairing')
    pair.chmod(0o600)
if behavior == 'exit':
    sys.exit(7)
ready = pathlib.Path(sys.argv[sys.argv.index('--ready-file') + 1])
if behavior != 'timeout':
    tmp = ready.with_suffix('.tmp')
    tmp.write_text(json.dumps(dict(schemaVersion=1, protocolVersion=1, version=('260603140506+0200-' + revision[:7] if revision else release), packageVersion=release,
        pid=os.getpid() if behavior != 'pid' else os.getppid(), ready=True,
        **(dict(channel='main', revision=revision if behavior != 'readyrevision' else '0' * 40) if revision else {{}}))))
    tmp.chmod(0o600 if behavior != 'mode' else 0o644)
    if behavior == 'symlink':
        ready.symlink_to(tmp)
    else:
        os.replace(tmp, ready)
if behavior == 'signalexit':
    time.sleep(0.2)
    os.kill(os.getpid(), signal.SIGUSR1)
while True:
    time.sleep(0.05)
'''.format(python=sys.executable, release=release, behavior=behavior, revision=revision, events=str(self.events))).encode()
        url = ('/main/client/' + revision if revision else '/client/' + release) + '/' + launcher.target() + '/herdr-webapp'
        self.assets[url] = body
        self.manifest = dict(schemaVersion=1, protocolVersion=1, version=release,
                             assets=[dict(target=launcher.target(), url=url, size=len(body), sha256=hashlib.sha256(body).hexdigest())])
        if revision:
            self.manifest.update(schemaVersion=2, channel='main', revision=revision)

    def launch(self, auto_update=True):
        child = subprocess.Popen([sys.executable, str(SCRIPT), '--relay-url', self.url,
                                  '--allow-http-loopback', '--cache-dir', str(self.root / 'cache'),
                                  '--state-dir', str(self.root / 'state'), '--poll-interval', '0.1',
                                  '--ready-timeout', '0.3', '--network-timeout', '1', '--no-install-core'] + (['--auto-update'] if auto_update else []),
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(child)
        return child

    def log(self):
        return self.events.read_text() if self.events.exists() else ''

    def wait_ready(self, release='1.0.0'):
        def ready():
            files = list((self.root / 'cache').glob('*/current.json'))
            return bool(files) and json.loads(files[0].read_text())['version'] == release
        eventually(ready)

    def test_old_main_compat_probe_only_falls_back_for_unsupported_option(self):
        binary = self.root / 'old-main'
        log = self.root / 'probe-flags'
        asset = dict(version='0.0.8', channel='main', revision='a' * 40)
        def script(package):
            binary.write_text('#!/bin/sh\nprintf "%s\\n" "$1" >> ' + str(log) + '\n'
                              + 'case "$1" in\n--package-version) ' + package + ';;\n'
                              + '--version) printf "herdr-webapp 0.0.8\\n";;\n'
                              + '--build-revision) printf "' + 'a' * 40 + '\\n";;\nesac\n')
            binary.chmod(0o700)
        script('exit 2')
        launcher.probe_release(binary, asset)
        self.assertEqual(log.read_text().splitlines(), ['--package-version', '--version', '--build-revision'])
        for package in ['printf "herdr-webapp 9.9.9\\n"', 'exit 1']:
            log.unlink()
            script(package)
            with self.assertRaises(launcher.Error):
                launcher.probe_release(binary, asset)
            self.assertEqual(log.read_text().splitlines(), ['--package-version'])

    def control(self, action, **kwargs):
        path = self.events.with_suffix('.control').read_text()
        with socket.socket(socket.AF_UNIX) as conn:
            conn.settimeout(2)
            conn.connect(path)
            conn.sendall(json.dumps(dict(schemaVersion=1, action=action, **kwargs)).encode() + b'\n')
            return json.loads(conn.recv(16384))

    def test_default_checks_only_then_intentional_update_reuses_socket(self):
        child = self.launch(auto_update=False)
        self.wait_ready()
        control = self.events.with_suffix('.control').read_text()
        self.publish('1.1.0')
        eventually(lambda: self.control('status')['status'] == 'available')
        self.assertNotIn('start 1.1.0', self.log())
        self.assertEqual(self.control('update', expectedVersion='9.9.9')['status'], 'failed')
        self.assertEqual(self.control('update', expectedVersion='1.1.0')['status'], 'updating')
        self.wait_ready('1.1.0')
        self.assertEqual(self.events.with_suffix('.control').read_text(), control)
        self.assertEqual(self.control('status')['currentVersion'], '1.1.0')
        self.assertIsNone(child.poll())

    def test_main_offers_revision_cas_and_reports_human_current_version(self):
        self.publish('1.0.0', revision='a' * 40)
        self.url += '/main'
        self.launch(auto_update=False)
        self.wait_ready()
        data = self.control('status')
        self.assertEqual(data['currentVersion'], '260603140506+0200-aaaaaaa')
        self.assertEqual(data['currentRevision'], 'a' * 40)
        self.publish('1.0.0', revision='b' * 40)
        eventually(lambda: self.control('status')['status'] == 'available')
        self.assertEqual(self.control('status')['availableRevision'], 'b' * 40)
        self.assertEqual(self.control('update', expectedVersion='1.0.0')['status'], 'failed')
        self.assertEqual(self.control('update', expectedVersion='1.0.0', expectedRevision='b' * 40)['status'], 'updating')
        eventually(lambda: self.control('status')['currentRevision'] == 'b' * 40)
        self.assertEqual(self.control('status')['currentVersion'], '260603140506+0200-bbbbbbb')

    def test_update_download_keeps_old_alive_and_retains_pairing(self):
        child = self.launch()
        self.wait_ready()
        pairing = next((self.root / 'state').glob('*/pairing-test'))
        inode = pairing.stat().st_ino
        self.downloading.clear()
        self.hold.clear()
        self.publish('1.1.0')
        self.assertTrue(self.downloading.wait(3))
        self.assertNotIn('stop 1.0.0', self.log())
        self.hold.set()
        self.wait_ready('1.1.0')
        self.publish('1.2.0')
        self.wait_ready('1.2.0')
        self.assertEqual(len(list((self.root / 'cache').glob('*/client-*'))), 2)
        self.assertEqual(pairing.stat().st_ino, inode)
        self.assertEqual(pairing.read_text(), 'preserve pairing')
        child.terminate()
        self.assertEqual(child.wait(5), -signal.SIGTERM)
        eventually(lambda: 'stop 1.2.0' in self.log())
        self.assertEqual(list((self.root / 'state').glob('*/.ready-*')), [])
        self.assertEqual(list((self.root / 'cache').glob('*/.download-*')), [])

    def test_failed_readiness_rolls_back_exact_child(self):
        for behavior in ('exit', 'timeout', 'pid', 'mode', 'symlink'):
            with self.subTest(behavior=behavior):
                child = self.launch()
                self.wait_ready()
                before = self.log().count('start 1.0.0')
                release = '2.0.' + str(('exit', 'timeout', 'pid', 'mode', 'symlink').index(behavior))
                self.publish(release, behavior)
                eventually(lambda: self.log().count('start 1.0.0') > before)
                self.assertIsNone(child.poll())
                self.assertNotIn('stop ' + release + ' ' + str(os.getpid()), self.log())
                child.terminate()
                child.wait(5)
                self.publish('1.0.0')

    def test_startup_upgrade_failure_restores_cached_release(self):
        child = self.launch()
        self.wait_ready()
        child.terminate()
        child.wait(5)
        before = self.log().count('start 1.0.0')
        self.publish('2.0.0', 'exit')
        other = self.launch()
        eventually(lambda: self.log().count('start 1.0.0') > before)
        self.assertIsNone(other.poll())

    def test_child_signal_is_propagated(self):
        self.publish('1.0.0', 'signalexit')
        child = self.launch()
        self.assertEqual(child.wait(5), -signal.SIGUSR1)
        self.assertEqual(list((self.root / 'state').glob('*/.ready-*')), [])

    def test_manifest_and_checksum_fail_closed(self):
        client = launcher.Client(self.url, 1)
        for mutation in ({'protocolVersion': 2}, {'schemaVersion': True}, {'version': '../evil'}, {'assets': []}):
            original = self.manifest
            self.manifest = dict(original, **mutation)
            with self.assertRaises(launcher.Error):
                client.manifest(launcher.target())
            self.manifest = original
        for key, value in [('url', 'https://evil.invalid/a'), ('url', '//evil/a'), ('url', '/client/../a'),
                           ('sha256', 'z' * 64), ('size', True), ('size', launcher.MAX_BINARY + 1)]:
            asset = self.manifest['assets'][0]
            old = asset[key]
            asset[key] = value
            with self.assertRaises(launcher.Error):
                client.manifest(launcher.target())
            asset[key] = old
        self.manifest['assets'][0]['sha256'] = '0' * 64
        child = self.launch()
        self.assertEqual(child.wait(5), 1)
        self.assertEqual(self.log(), '')
        self.assertEqual(list((self.root / 'cache').glob('*/.download-*')), [])

    def test_downgrade_rejected_and_single_owner(self):
        child = self.launch()
        self.wait_ready()
        second = self.launch()
        self.assertEqual(second.wait(5), 1)
        self.assertIsNone(child.poll())
        self.publish('0.9.0')
        time.sleep(0.3)
        self.assertNotIn('start 0.9.0', self.log())
        child.terminate()
        child.wait(5)
        third = self.launch()
        self.wait_ready()
        time.sleep(0.2)
        self.assertIsNone(third.poll())
        self.assertNotIn('start 0.9.0', self.log())

    def test_origin_and_redirects(self):
        for value in ('http://example.com', 'https://user:pass@example.com', 'https://example.com/a',
                      'https://example.com?token=secret', 'https://evil\\@example.com', 'https://example.com\n'):
            with self.assertRaises(launcher.Error):
                launcher.origin(value, True)
        self.assertEqual(launcher.origin('https://EXAMPLE.com:443/'), 'https://example.com')
        self.assertEqual(launcher.origin(self.url, True), self.url)
        import io
        with self.assertRaises(launcher.Error):
            launcher.Client(self.url, 1).stream('/redirect', 100, io.BytesIO())

    def test_main_origin_paths_manifest_and_no_stable_fallback(self):
        self.publish('0.0.8', revision='a' * 40)
        endpoint = launcher.origin(self.url + '/main/', True)
        self.assertEqual(endpoint, self.url + '/main')
        asset = launcher.Client(endpoint, 1).manifest(launcher.target())
        self.assertEqual(asset['revision'], 'a' * 40)
        self.assertTrue(asset['url'].startswith('/main/client/'))
        for bad in ('/main/../', '/main//', '/%6dain', '/main?x', '/main#', '//main', '/main/other'):
            with self.assertRaises(launcher.Error):
                launcher.origin(self.url + bad, True)
        client = launcher.Client(endpoint, 1)
        for mutation in ({'revision': 'a' * 39}, {'revision': 'A' * 40}, {'channel': 'stable'}, {'schemaVersion': 1}):
            old = self.manifest
            self.manifest = dict(old, **mutation)
            with self.assertRaises(launcher.Error):
                client.manifest(launcher.target())
            self.manifest = old
        self.manifest['assets'][0]['url'] = '/client/0.0.8/' + launcher.target() + '/herdr-webapp'
        with self.assertRaises(launcher.Error):
            client.manifest(launcher.target())
        self.manifest['assets'] = []
        with self.assertRaisesRegex(launcher.Incompatible, 'available targets:'):
            client.manifest(launcher.target())

    def test_main_same_version_revision_updates_and_force_push(self):
        self.url += '/main'
        self.publish('0.0.8', revision='f' * 40)
        child = self.launch()
        def revision_ready(revision):
            files = list((self.root / 'cache').glob('*/current.json'))
            return bool(files) and json.loads(files[0].read_text()).get('revision') == revision
        eventually(lambda: revision_ready('f' * 40))
        self.publish('0.0.8', revision='1' * 64)
        eventually(lambda: revision_ready('1' * 64))
        self.assertEqual(len(list((self.root / 'cache').glob('*/client-*'))), 2)
        self.publish('0.0.8', revision='f' * 40)
        eventually(lambda: revision_ready('f' * 40))
        child.terminate()
        child.wait(5)
        self.assertEqual(len(list((self.root / 'state').glob('*/pairing-test'))), 1)
        # Offline startup uses the exact private revision cache, not stable's version key.
        self.manifest = None
        again = self.launch()
        eventually(lambda: self.log().count('start 0.0.8') >= 4)
        self.assertIsNone(again.poll())

    def test_main_revision_probe_and_readiness_fail_closed(self):
        self.url += '/main'
        for behavior in ('revision', 'readyrevision'):
            with self.subTest(behavior=behavior):
                self.publish('0.0.8', behavior, revision='a' * 40)
                child = self.launch()
                self.assertEqual(child.wait(5), 1)
                self.assertEqual(list((self.root / 'cache').glob('*/current.json')), [])

    def test_main_same_version_bad_revision_readiness_rolls_back(self):
        self.url += '/main'
        self.publish('0.0.8', revision='a' * 40)
        child = self.launch()
        self.wait_ready('0.0.8')
        pairing = next((self.root / 'state').glob('*/pairing-test'))
        inode = pairing.stat().st_ino
        before = self.log().count('start 0.0.8')
        self.publish('0.0.8', 'readyrevision', revision='b' * 40)
        eventually(lambda: self.log().count('start 0.0.8') >= before + 2)
        current = next((self.root / 'cache').glob('*/current.json'))
        self.assertEqual(json.loads(current.read_text())['revision'], 'a' * 40)
        self.assertEqual(pairing.stat().st_ino, inode)
        self.assertIsNone(child.poll())

    def test_main_cache_state_identity_and_bootstrap(self):
        from types import SimpleNamespace
        args = dict(allow_http_loopback=True, cache_dir=self.root / 'cache', state_dir=self.root / 'state', network_timeout=1)
        stable = launcher.Supervisor(SimpleNamespace(relay_url=self.url, **args))
        main = launcher.Supervisor(SimpleNamespace(relay_url=self.url + '/main/', **args))
        self.assertNotEqual(stable.cache, main.cache)
        self.assertNotEqual(stable.state, main.state)
        self.assertEqual(main.relay, self.url + '/main')
        self.assertEqual(stable.cache.name, hashlib.sha256(self.url.encode()).hexdigest()[:32])
        tools = self.root / 'tools'
        tools.mkdir()
        downloaded = self.root / 'download-url'
        invoked = self.root / 'python-args'
        curl = tools / 'curl'
        curl.write_text('#!/bin/sh\nfor last do :; done\nprintf "%s" "$last" > "$DOWNLOADED"\nwhile [ "$1" != --output ]; do shift; done\nshift\nprintf "stub" > "$1"\n')
        curl.chmod(0o700)
        python = tools / 'python3'
        python.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$INVOKED"\n')
        python.chmod(0o700)
        wrapper = self.root / 'run-main.sh'
        wrapper.write_text(SCRIPT.with_name('run-relay.sh').read_text().replace('__HERDR_RELAY_ORIGIN__', 'https://example.com/main'))
        env = dict(os.environ, PATH=str(tools) + ':' + os.environ['PATH'], DOWNLOADED=str(downloaded), INVOKED=str(invoked))
        result = subprocess.run(['sh', str(wrapper)], env=env, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(downloaded.read_text(), 'https://example.com/main/client/launcher.py')
        self.assertTrue(invoked.read_text().endswith('--relay-url\nhttps://example.com/main\n'))

    def test_private_files_and_lock_links(self):
        private = self.root / 'private'
        launcher.private_dir(private)
        self.assertEqual(private.stat().st_mode & 0o777, 0o700)
        unsafe = private / 'unsafe'
        unsafe.write_text('x')
        with self.assertRaises(launcher.Error):
            launcher.safe_open(unsafe)
        unsafe.chmod(0o600)
        link = private / 'link'
        link.symlink_to(unsafe)
        with self.assertRaises(OSError):
            launcher.safe_open(link)
        link.unlink()
        os.link(unsafe, link)
        with self.assertRaises(launcher.Error):
            launcher.safe_open(unsafe)
        linked_dir = self.root / 'linked'
        linked_dir.symlink_to(private)
        with self.assertRaises(launcher.Error):
            launcher.private_dir(linked_dir / 'nested')

    def test_wrapper_cleanup_and_download_failure(self):
        tools = self.root / 'tools'
        tools.mkdir()
        scratch = self.root / 'tmp'
        scratch.mkdir(mode=0o700)
        marker = self.root / 'python-ran'
        curl = tools / 'curl'
        curl.write_text('#!/bin/sh\nwhile [ "$1" != --output ]; do shift; done\nshift\nprintf "partial" > "$1"\nexit "${CURL_STATUS:-0}"\n')
        curl.chmod(0o700)
        python = tools / 'python3'
        python.write_text('#!/bin/sh\nprintf ran > "$MARKER"\n')
        python.chmod(0o700)
        wrapper = self.root / 'run.sh'
        wrapper.write_text(SCRIPT.with_name('run-relay.sh').read_text().replace('__HERDR_RELAY_ORIGIN__', 'https://example.com:443'))
        env = dict(os.environ, PATH=str(tools) + ':' + os.environ['PATH'], TMPDIR=str(scratch), MARKER=str(marker))
        for status in ('22', '0'):
            result = subprocess.run(['sh', str(wrapper)], env=dict(env, CURL_STATUS=status), capture_output=True, timeout=5)
            self.assertEqual(result.returncode, int(status), result.stderr)
            self.assertEqual(marker.exists(), status == '0')
            self.assertEqual(list(scratch.iterdir()), [])

    def test_cached_tampering_is_not_executed(self):
        child = self.launch()
        self.wait_ready()
        child.terminate()
        child.wait(5)
        binary = next((self.root / 'cache').glob('*/client-*'))
        binary.write_bytes(b'#!/bin/sh\nexit 0\n')
        before = self.log()
        other = self.launch()
        self.assertEqual(other.wait(5), 1)
        self.assertEqual(self.log(), before)


if __name__ == '__main__':
    unittest.main()
