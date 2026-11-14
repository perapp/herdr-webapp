#!/usr/bin/env python3
"""Offline supervisor tests. Git commits exist only in fresh private temp dirs."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("main_channel", Path(__file__).with_name("main-channel.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

A, B, C = "a" * 40, "b" * 40, "c" * 40


class Harness(m.Manager):
    def __init__(self, args):
        super().__init__(args)
        self.tip = A
        self.built = []
        self.bad = set()
        self.bad_probe = False

    def fetch(self):
        return self.tip

    def build(self, sha):
        self.built.append(sha)
        if sha in self.bad:
            raise m.Failure("test_build")
        binary = self.state / "fake-binary"
        binary.write_text("#!/bin/sh\nprintf '%s\\n' '" + sha + "'\n")
        binary.chmod(0o700)
        self.publish_artifact(sha, binary, "0.0.8")
        binary.unlink()

    def child(self, binary, catalog, sha, sock):
        return self.spawn(["python3", "-c", "import time; time.sleep(100)"])

    def probe(self, sock, sha, manifest):
        if self.bad_probe:
            raise m.Failure("test_probe")


class Tests(unittest.TestCase):
    def setUp(self):
        self.old_umask = os.umask(0o077)
        self.temp = tempfile.TemporaryDirectory(prefix="mc-")
        self.root = Path(self.temp.name)
        self.managers = []
        self.disk_patch = mock.patch.object(m.shutil, "disk_usage", return_value=mock.Mock(free=9 * 1024**3))
        self.disk_patch.start()

    def tearDown(self):
        for manager in self.managers:
            manager.close()
        self.temp.cleanup()
        self.disk_patch.stop()
        os.umask(self.old_umask)

    def args(self, state=None):
        return m.parser().parse_args(["--state-dir", str(state or self.root / "state"),
                                      "--public-origin", "https://relay.example"])

    def manager(self, cls=Harness, state=None):
        manager = cls(self.args(state))
        self.managers.append(manager)
        return manager

    def test_cli_validation(self):
        a = self.args()
        m.validate_args(a)
        self.assertEqual((a.interval_seconds, a.build_jobs), (300, 2))
        for field, value in [("repository", "https://user:secret@example/repo"),
                             ("repository", "file:///tmp/repo"), ("branch", "../main"),
                             ("public_origin", "https://host/main/"),
                             ("public_origin", "http://host"), ("public_origin", "https://host:bad"),
                             ("build_jobs", 0), ("build_jobs", 17),
                             ("turn_ttl_seconds", 29), ("turn_ttl_seconds", 601),
                             ("interval_seconds", 0)]:
            with self.subTest(field=field, value=value):
                a = self.args()
                setattr(a, field, value)
                with self.assertRaises(m.Failure):
                    m.validate_args(a)
        a = self.args()
        a.public_origin = "http://127.0.0.1:9000"
        a.allow_http_loopback = True
        m.validate_args(a)

    def test_unchanged_new_force_update_and_retention(self):
        x = self.manager()
        x.update()
        record = m.read_json(x.state / "active.json")
        self.assertEqual(record["revision"], A)
        self.assertEqual(Path(record["socket"]).name, record["socket"])
        self.assertNotIn("/", record["socket"])
        x.update()
        self.assertEqual(x.built, [A])
        for tip in (B, C, "d" * 40):
            x.tip = tip
            x.update()
        self.assertEqual(x.current[2], "d" * 40)
        # Force update to an older, non-descendant exact SHA: cached artifact reused.
        x.tip = A
        x.update()
        self.assertEqual(x.current[2], A)
        self.assertEqual(x.built.count(A), 1)
        x.retired = [(0, *entry[1:]) for entry in x.retired]
        x.maintenance()
        x.prune()
        self.assertLessEqual(len(list(x.artifacts.iterdir())), 3)

    def test_failure_retry_coalesces_new_tip_and_bounds_retry(self):
        x = self.manager()
        x.update()
        old = m.read_json(x.state / "active.json")
        x.tip = B
        x.bad.add(B)
        x.update()
        self.assertEqual(m.read_json(x.state / "active.json"), old)
        x.update()
        self.assertEqual(x.built.count(B), 1)
        for _ in range(4):
            x.failures[B]["after"] = 0
            x.update()
        self.assertEqual(x.built.count(B), 3)
        # Newest observed tip is not blocked by an older failed SHA.
        x.tip = C
        x.update()
        self.assertEqual(x.current[2], C)
        status = (x.state / "status.json").read_text()
        self.assertNotIn(str(self.root), status)
        self.assertNotIn("test_build", status)

    def test_readiness_rollback_and_atomic_publication(self):
        x = self.manager()
        x.update()
        old = m.read_json(x.state / "active.json")
        x.build(B)
        x.bad_probe = True
        # Advance only the readiness deadline, without making the test wait 30 sec.
        artifact = x.artifact(B)
        with mock.patch.object(x, "artifact", return_value=artifact), \
                mock.patch.object(x, "maintenance"), \
                mock.patch.object(m.time, "monotonic", side_effect=[0, 31]):
            with self.assertRaises(m.Failure):
                x.activate(B)
        self.assertEqual(m.read_json(x.state / "active.json"), old)
        self.assertIsNone(x.current[0].poll())
        self.assertEqual(len(x.groups), 1)
        x.bad_probe = False
        real = m.atomic_json
        def broken(path, data):
            if path.name == "active.json":
                raise OSError("test private path/credential")
            real(path, data)
        with mock.patch.object(m, "atomic_json", side_effect=broken):
            with self.assertRaises(OSError):
                x.activate(B)
        self.assertEqual(m.read_json(x.state / "active.json"), old)
        self.assertEqual(len(x.groups), 1)

    def test_offline_boot_hash_and_provenance_validation(self):
        x = self.manager()
        x.update()
        state = x.state
        x.close()
        self.managers.remove(x)
        y = self.manager(state=state)
        y.boot()
        self.assertEqual(y.current[2], A)
        binary, _, _ = y.artifact(A)
        binary.write_text("#!/bin/sh\necho wrong\n")
        with self.assertRaises(m.Failure):
            y.artifact(A)

    def test_permissions_lock_symlink_and_long_path(self):
        x = self.manager()
        with self.assertRaises(m.Failure):
            Harness(self.args())
        bad = self.root / "bad"
        bad.mkdir(mode=0o755)
        bad.chmod(0o755)
        with self.assertRaises(m.Failure):
            Harness(self.args(bad))
        link = self.root / "link"
        link.symlink_to(x.state, target_is_directory=True)
        with self.assertRaises(m.Failure):
            Harness(self.args(link))
        long = self.root / ("x" * 90)
        with self.assertRaises(m.Failure):
            Harness(self.args(long))
        x.update()
        _, catalog, _ = x.artifact(A)
        manifest = catalog / "manifest.json"
        manifest.chmod(0o644)
        with self.assertRaises(m.Failure):
            x.artifact(A)
        manifest.chmod(0o600)
        alias = catalog / "alias"
        alias.hardlink_to(manifest)
        with self.assertRaises(m.Failure):
            x.artifact(A)

    def test_shutdown_only_owned_groups_and_crash_backoff(self):
        unrelated = subprocess.Popen(["python3", "-c", "import time;time.sleep(100)"],
                                     start_new_session=True)
        try:
            x = self.manager()
            x.update()
            owned = x.current[0]
            os.killpg(owned.pid, signal.SIGKILL)
            owned.wait()
            x.maintenance()
            self.assertIsNone(x.current)
            self.assertGreater(x.restart_at, time.monotonic())
            x.activate(A)
            owned = x.current[0]
            x.close()
            self.managers.remove(x)
            self.assertIsNotNone(owned.poll())
            self.assertIsNone(unrelated.poll())
            self.assertIsNone(m.read_json(x.state / "active.json")["socket"])
        finally:
            unrelated.terminate()
            unrelated.wait()

    def test_command_deadline_and_shutdown(self):
        x = self.manager()
        with self.assertRaises(m.Failure):
            x.command(["python3", "-c", "import time;time.sleep(100)"], timeout=0.02)
        self.assertFalse(x.groups)
        x.stopping = True
        with self.assertRaises(m.Failure):
            x.command(["python3", "-c", "import time;time.sleep(100)"])
        self.assertFalse(x.groups)

    @unittest.skipUnless(shutil.which("git"), "git needed")
    def test_local_git_remote_exact_tip_unchanged_and_force_update(self):
        # The internal test seam replaces only transport URL validation: production
        # accepts HTTPS, tests exercise real Git entirely inside this temp dir.
        repo = self.root / "repo"
        repo.mkdir()
        def git(*args):
            return subprocess.check_output(["git", *args], cwd=repo,
                                           stderr=subprocess.DEVNULL).decode().strip()
        git("init", "-b", "main")
        git("config", "user.name", "Isolated Test")
        git("config", "user.email", "test@example.invalid")
        (repo / "one").write_text("one")
        git("add", ".")
        git("commit", "-m", "one")
        first = git("rev-parse", "HEAD")
        x = self.manager(m.Manager)
        x.args.repository = repo.as_uri()
        self.assertEqual(x.fetch(), first)
        self.assertEqual(x.fetch(), first)
        (repo / "one").write_text("two")
        git("commit", "-am", "two")
        second = git("rev-parse", "HEAD")
        self.assertEqual(x.fetch(), second)
        git("reset", "--hard", first)
        self.assertEqual(x.fetch(), first)
        x.args.repository = (self.root / "missing").as_uri()
        with self.assertRaises(m.Failure):
            x.fetch()

    def test_disk_reserve_before_build_cleans_caches_without_touching_current(self):
        x = self.manager()
        x.update()
        owned = x.current[0]
        x.checkout.mkdir(mode=0o700)
        for rel in ("source/target", "source/frontend/node_modules", "cargo-cache", "npm-cache"):
            path = x.state / rel
            path.mkdir(parents=True, exist_ok=True)
            (path / "cached").write_bytes(b"cache")
        with mock.patch.object(m.shutil, "disk_usage", return_value=mock.Mock(free=m.MIN_FREE_BYTES - 1)), \
                mock.patch.object(x, "command") as command:
            with self.assertRaisesRegex(m.Failure, "disk_reserve"):
                m.Manager.build(x, B)
            # Only cleanup Git operations are allowed; no checkout/npm/cargo.
            self.assertEqual([args[0][1] for args, _ in command.call_args_list], ["reflog", "gc"])
        self.assertIsNone(owned.poll())
        self.assertEqual(m.read_json(x.state / "active.json")["revision"], A)
        for rel in ("source/target", "source/frontend/node_modules", "cargo-cache", "npm-cache"):
            self.assertFalse((x.state / rel).exists())
        self.assertFalse(x.building)

    def test_disk_reserve_during_command_stops_only_build_group(self):
        x = self.manager()
        x.update()
        serving = x.current[0]
        x.building = True
        with mock.patch.object(m.shutil, "disk_usage", return_value=mock.Mock(free=m.MIN_FREE_BYTES - 1)):
            with self.assertRaisesRegex(m.Failure, "disk_reserve"):
                x.command(["python3", "-c", "import time; time.sleep(100)"])
        x.building = False
        self.assertEqual(x.groups, {serving})
        self.assertIsNone(serving.poll())

    def test_private_child_environment_and_exact_cli(self):
        x = self.manager()
        hostile = {"HERDR_WEB_PORT": "1234", "HERDR_WEB_BIND": "0.0.0.0",
                   "HERDR_MAIN_ENTRY_PID": "1", "HERDR_MAIN_REPOSITORY": "private-repository",
                   "GIT_CONFIG_COUNT": "1", "GIT_TOKEN": "secret", "SSH_AUTH_SOCK": "/private/agent",
                   "AWS_SECRET_ACCESS_KEY": "secret", "PATH": "/usr/bin", "LANG": "C.UTF-8"}
        with mock.patch.dict(os.environ, hostile), mock.patch.object(x, "spawn") as spawn:
            m.Manager.child(x, Path("/binary"), Path("/catalog"), A, Path("/socket"))
        argv = spawn.call_args.args[0]
        self.assertEqual(argv, ["/binary", "--public-origin", x.args.public_origin, "serve-relay",
                               "--channel-base-path", "/main/", "--channel-revision", A,
                               "--unix-socket", "/socket", "--release-dir", "/catalog"])
        env = spawn.call_args.kwargs["env"]
        for key in hostile:
            if key not in ("PATH", "LANG"):
                self.assertNotIn(key, env)
        self.assertEqual(env["HERDR_MAIN_PARENT_PID"], str(os.getpid()))
        self.assertEqual(env["LANG"], "C.UTF-8")

    def test_entry_death_stops_owned_children_from_run_and_build(self):
        x = self.manager()
        x.update()
        serving = x.current[0]
        # Maintenance during a build exits the command promptly; run() then
        # tears down the child as well, without signalling any foreign PID.
        x.building = True
        with mock.patch.object(m.os, "getppid", return_value=x.entry_pid + 1):
            with self.assertRaisesRegex(m.Failure, "entry_exited"):
                x.command(["python3", "-c", "import time; time.sleep(100)"])
        self.assertTrue(x.stopping)
        self.assertEqual(x.groups, {serving})
        x.building = False
        x.stopping = False
        with mock.patch.object(x, "boot"), \
                mock.patch.object(m.os, "getppid", return_value=x.entry_pid + 1):
            with self.assertRaisesRegex(m.Failure, "entry_exited"):
                x.run()
        self.managers.remove(x)
        self.assertIsNotNone(serving.poll())
        self.assertFalse(x.groups)

    def test_build_attempt_immediately_refetches_newest_tip(self):
        x = self.manager()
        attempts = []
        def update():
            attempts.append(True)
            if len(attempts) == 1:
                return True
            x.stopping = True
            return False
        with mock.patch.object(x, "boot"), mock.patch.object(x, "update", side_effect=update), \
                mock.patch.object(m.time, "monotonic", return_value=100), \
                mock.patch.object(m.time, "sleep", side_effect=[None, None, AssertionError("refetch delayed")]):
            x.run()
        self.managers.remove(x)
        self.assertEqual(len(attempts), 2)

    def test_build_order_jobs_revision_and_cache_cleanup(self):
        x = self.manager(m.Manager)
        x.checkout.mkdir(mode=0o700)
        (x.checkout / "frontend").mkdir(mode=0o700)
        (x.checkout / "Cargo.toml").write_text('[package]\nversion="0.0.8"\n')
        calls = []
        def command(argv, cwd=None, env=None, timeout=3600, capture=False):
            calls.append((argv, cwd, env))
            if argv[:2] == ["cargo", "build"]:
                binary = x.checkout / "target/release/herdr-webapp"
                binary.parent.mkdir(parents=True)
                binary.write_text("fake compiled binary")
                for cache in ("cargo-cache", "npm-cache"):
                    (x.state / cache).mkdir()
            if argv[-1] == "--build-revision":
                return A
            return ""
        with mock.patch.object(x, "command", side_effect=command), \
                mock.patch.object(m.shutil, "disk_usage", return_value=mock.Mock(free=m.MIN_FREE_BYTES + 1024**3)), \
                mock.patch.dict(os.environ, {"CARGO_BUILD_TARGET": "wrong-target", "CARGO_TARGET_DIR": "/wrong"}):
            x.build(A)
        builds = [(a, d, e) for a, d, e in calls if a[0] in ("npm", "cargo")]
        self.assertEqual([a for a, _, _ in builds], [["npm", "ci"], ["npm", "run", "build"],
                                                     ["cargo", "build", "--release", "--locked"]])
        for _, _, env in builds:
            self.assertEqual(env["CARGO_BUILD_JOBS"], "2")
            self.assertEqual(env["CARGO_INCREMENTAL"], "0")
            self.assertEqual(env["HERDR_BUILD_REVISION"], A)
            self.assertNotIn("CARGO_BUILD_TARGET", env)
            self.assertEqual(env["CARGO_TARGET_DIR"], str(x.checkout / "target"))
        self.assertLess(x.build_budget, 1024**3 + 4096)
        self.assertFalse((x.checkout / "target").exists())
        self.assertFalse((x.state / "cargo-cache").exists())
        self.assertFalse((x.state / "npm-cache").exists())
        self.assertTrue((x.artifacts / A / "catalog/manifest.json").exists())

    def test_native_only_and_catalog_bytes(self):
        x = self.manager()
        with mock.patch.object(m.platform, "system", return_value="Windows"):
            with self.assertRaises(m.Failure):
                m.native_target()
        x.build(A)
        _, catalog, manifest = x.artifact(A)
        self.assertEqual(len(manifest["assets"]), 1)
        self.assertEqual(manifest["revision"], A)
        for path in catalog.rglob("*"):
            self.assertEqual(path.stat().st_mode & 0o077, 0)
        data = m.read_json(catalog / "manifest.json")
        data["revision"] = B
        m.atomic_json(catalog / "manifest.json", data)
        with self.assertRaises(m.Failure):
            x.artifact(A)

    def test_real_uds_websocket_challenge_and_duplicate_headers(self):
        path = self.root / "probe.sock"
        server = m.socket.socket(m.socket.AF_UNIX, m.socket.SOCK_STREAM)
        server.bind(str(path))
        server.listen(2)
        server.settimeout(5)
        errors = []
        received = []
        payload = b'{"type":"challenge","nonce":"0123456789abcdef0123456789abcdef"}'
        def serve():
            try:
                for _ in range(2):
                    conn, _ = server.accept()
                    with conn:
                        conn.settimeout(3)
                        data = b""
                        while b"\r\n\r\n" not in data:
                            chunk = conn.recv(4096)
                            if not chunk:
                                raise AssertionError("incomplete request")
                            data += chunk
                        received.append(data)
                        if data.count(b"Origin: ") == 2:
                            conn.sendall(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                        else:
                            conn.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                                         b"Connection: Upgrade\r\n\r\n" + bytes([0x81, len(payload)]) + payload)
            except Exception as exc:
                errors.append(exc)
            finally:
                server.close()
        thread = m.threading.Thread(target=serve, daemon=True)
        thread.start()
        try:
            code, _, body = m.request(path, "/main/relay/socket", headers={"Origin": "https://relay.example"})
            self.assertEqual(code, 101)
            self.assertEqual(body, payload)
            code, _, _ = m.request(path, "/main/relay/socket", headers=[("Origin", "https://relay.example"),
                                                                       ("Origin", "https://relay.example")])
            self.assertEqual(code, 403)
        finally:
            thread.join(timeout=6)
            server.close()
        self.assertFalse(thread.is_alive())
        self.assertFalse(errors)
        self.assertEqual(received[-1].count(b"Origin: "), 2)

    def test_probe_contract_and_origin(self):
        x = self.manager()
        x.build(A)
        _, _, manifest = x.artifact(A)
        asset = manifest["assets"][0]
        binary = (x.artifacts / A / "catalog" / A / asset["target"] / "herdr-webapp").read_bytes()
        seen = []
        mode_changes = {}
        challenge = {"type": "challenge", "nonce": "a" * 32}
        html_asset = ["./assets/index.js"]
        def response(sock, url, limit=1024 * 1024, headers=None):
            seen.append(url)
            if not url.startswith("/main/"):
                return 404, {}, b""
            if url.endswith("healthz"):
                return 200, {}, b'{"ready":true,"protocolVersion":1}'
            if url.endswith("api/mode"):
                mode = dict(mode="relay", channel="main", basePath="/main/", revision=A,
                            protocolVersion=1, entrypointProtocolVersion=1,
                            publicOrigin=x.args.public_origin, version="0.0.8")
                mode.update(mode_changes)
                return 200, {}, json.dumps(mode).encode()
            if url == "/main/":
                return 200, {}, ('<html><script src="' + html_asset[0] + '"></script></html>').encode()
            if url.endswith("index.js"):
                return 200, {}, b"javascript"
            if url.endswith("manifest.json"):
                return 200, {}, json.dumps(manifest).encode()
            if url == asset["url"]:
                return 200, {}, binary
            if isinstance(headers, list) or headers["Origin"] != x.args.public_origin:
                return 403, {}, b""
            accept = m.base64.b64encode(m.hashlib.sha1((headers["Sec-WebSocket-Key"] +
                       "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
            return 101, {"Sec-WebSocket-Accept": accept, "Upgrade": "websocket",
                         "Connection": "Upgrade"}, json.dumps(challenge).encode()
        with mock.patch.object(m, "request", side_effect=response):
            m.Manager.probe(x, x.state / "s.sock", A, manifest)
        self.assertEqual(seen.count("/main/relay/socket"), 3)
        self.assertIn("/main/assets/index.js", seen)
        self.assertIn(asset["url"], seen)
        for html_asset[0] in ["/main/assets/index.js", "./assets/index.js"]:
            with mock.patch.object(m, "request", side_effect=response):
                m.Manager.probe(x, x.state / "s.sock", A, manifest)
        for html_asset[0] in ["/assets/index.js", "../assets/index.js", "https://evil.example/assets/index.js", "//evil.example/assets/index.js", "./assets/%2e%2e/index.js"]:
            with mock.patch.object(m, "request", side_effect=response):
                with self.assertRaises(m.Failure):
                    m.Manager.probe(x, x.state / "s.sock", A, manifest)
        html_asset[0] = "./assets/index.js"
        for incompatible in (None, 2, "1"):
            mode_changes["entrypointProtocolVersion"] = incompatible
            with mock.patch.object(m, "request", side_effect=response):
                with self.assertRaises(m.Failure):
                    m.Manager.probe(x, x.state / "s.sock", A, manifest)
        mode_changes.clear()
        challenge["type"] = "not-a-challenge"
        with mock.patch.object(m, "request", side_effect=response):
            with self.assertRaises(m.Failure):
                m.Manager.probe(x, x.state / "s.sock", A, manifest)


if __name__ == "__main__":
    unittest.main()
