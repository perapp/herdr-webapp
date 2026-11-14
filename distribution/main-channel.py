#!/usr/bin/env python3
"""Private, opt-in trusted-source main supervisor (Python 3.11+, POSIX).

Requires git, npm/Node and the host Rust toolchain. This executes trusted source,
not a sandbox. Use OS resource limits for memory/CPU and a filesystem quota for
hard disk limits. Keep state on a local filesystem; no other process may mutate
it. Three artifacts are retained; build caches are removed after every attempt,
Git is shallow/pruned. Source plus managed caches have a 6 GiB soft budget,
further limited to initial available space minus a 2 GiB free-space reserve.
The reserve is checked before builds and throughout running build commands;
quotas are still required to prevent transient overshoot between checks. Builds
and fetches have deadlines. No Core commands or CI/tag operations are performed.
Polling is serialized with builds, not parallel: after each build attempt the
newest branch tip is fetched immediately, then ordinary jittered polling resumes.
Child relay environment is allowlisted (no Git/SSH or HERDR configuration); this
is not credential isolation from trusted code running under the same UID.
Only the supervisor's own process groups are signalled. Parent entry PID changes
terminate the supervisor and its groups; the relay receives HERDR_MAIN_PARENT_PID
so it can exit if this supervisor itself dies. PID values are never used to kill
foreign processes. Activation disconnects
old streams after at most ten seconds: clients must reconnect, not replay input.

active.json is an atomic private routing record. Socket is a validated leaf in
state-dir; use a short state path (<=68 bytes, socket leaf <=40 bytes). No credentials,
command output or exception text is logged. Existing Git credential helpers work.
"""
from __future__ import annotations

import argparse
import base64
import fcntl
import hashlib
import http.client
import json
import os
from pathlib import Path
import platform
import random
import re
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
import urllib.parse

SHA = re.compile(r"[0-9a-f]{40}\Z")
SEMVER = re.compile(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?\Z")
MAX_BINARY = 128 * 1024 * 1024
MAX_WORKSPACE = 6 * 1024**3
MIN_FREE_BYTES = 2 * 1024**3


class Failure(Exception):
    """Only fixed, public error codes belong in this exception."""


def guard(path: Path, directory=False, executable=False):
    s = path.lstat()
    expected = stat.S_ISDIR(s.st_mode) if directory else stat.S_ISREG(s.st_mode)
    if (not expected or s.st_uid != os.geteuid() or s.st_mode & 0o077
            or (not directory and s.st_nlink != 1)
            or (executable and not s.st_mode & 0o100)):
        raise Failure("unsafe_permissions")
    return s


def private_dir(path):
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        pass
    guard(path, directory=True)


def atomic_json(path, value):
    fd, name = tempfile.mkstemp(prefix=".publish-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(value, f, separators=(",", ":"), sort_keys=True)
            f.flush()
            os.fsync(f.fileno())
        os.replace(name, path)
        d = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(d)
        finally:
            os.close(d)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def read_json(path):
    s = guard(path)
    if s.st_size > 65536:
        raise Failure("invalid_metadata")
    value = json.loads(path.read_text())
    if not isinstance(value, dict):
        raise Failure("invalid_metadata")
    return value


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def native_target():
    key = (platform.system(), platform.machine())
    targets = {("Linux", "x86_64"): "x86_64-unknown-linux-gnu",
               ("Linux", "aarch64"): "aarch64-unknown-linux-gnu",
               ("Darwin", "arm64"): "aarch64-apple-darwin"}
    if key not in targets or (key[0] == "Linux" and platform.libc_ver()[0] != "glibc"):
        raise Failure("unsupported_host")
    return targets[key]


class UnixHTTP(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__("localhost", timeout=2)
        self.path = str(path)

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)


def request(path, url, limit=1024 * 1024, headers=None):
    c = UnixHTTP(path)
    timer = None
    r = None
    try:
        c.connect()
        wire = c.sock
        def expire():
            try:
                wire.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        # Socket timeouts alone do not bound a slow trickle response body.
        timer = threading.Timer(5, expire)
        timer.daemon = True
        timer.start()
        if isinstance(headers, list):
            # Preserve duplicate headers for exact-Origin negative probes.
            c.putrequest("GET", url)
            for name, value in headers:
                c.putheader(name, value)
            c.endheaders()
        else:
            c.request("GET", url, headers=headers or {})
        r = c.getresponse()
        if r.status == 101:
            # The relay must actually speak its application protocol, not merely
            # return an upgrade header. Read exactly one bounded server text frame.
            def exact(n):
                data = r.fp.read(n)
                if len(data) != n:
                    raise Failure("websocket_truncated")
                return data
            first, second = exact(2)
            if first != 0x81 or second & 0x80:
                raise Failure("websocket_frame_invalid")
            length = second & 0x7f
            if length == 127:
                raise Failure("websocket_frame_invalid")
            if length == 126:
                length = int.from_bytes(exact(2), "big")
            if length > 4096:
                raise Failure("websocket_frame_invalid")
            body = exact(length)
        else:
            body = r.read(limit + 1)
        if len(body) > limit:
            raise Failure("probe_too_large")
        return r.status, dict(r.getheaders()), body
    finally:
        if timer:
            timer.cancel()
        if r:
            r.close()
        c.close()


def validate_args(a):
    parsed = urllib.parse.urlsplit(a.public_origin)
    if (parsed.scheme not in ("https", "http") or not parsed.hostname
            or parsed.username or parsed.password or parsed.path or parsed.query
            or parsed.fragment or any(c.isspace() for c in a.public_origin)
            or (parsed.scheme == "http" and not (a.allow_http_loopback and
                parsed.hostname in ("localhost", "127.0.0.1", "::1")))):
        raise Failure("invalid_origin")
    try:
        parsed.port
    except ValueError:
        raise Failure("invalid_origin") from None
    repo = urllib.parse.urlsplit(a.repository)
    if (repo.username or repo.password or repo.query or repo.fragment
            or repo.scheme != "https" or not repo.hostname):
        raise Failure("invalid_repository")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._/-]*", a.branch) or ".." in a.branch or "//" in a.branch:
        raise Failure("invalid_branch")
    if a.interval_seconds < 1 or not 1 <= a.build_jobs <= 16 or not 30 <= a.turn_ttl_seconds <= 600:
        raise Failure("invalid_limits")
    if a.turn_secret_file:
        guard(Path(a.turn_secret_file))
    if bool(a.turn_url) != bool(a.turn_secret_file):
        raise Failure("invalid_turn")


class Manager:
    # Tests override build/fetch/spawn/probe with internal methods, never CLI hooks.
    def __init__(self, args):
        self.args = args
        try:
            self.entry_pid = int(os.environ.get("HERDR_MAIN_ENTRY_PID", os.getppid()))
        except ValueError:
            raise Failure("invalid_entry_pid") from None
        if self.entry_pid < 1:
            raise Failure("invalid_entry_pid")
        self.state = Path(os.path.abspath(args.state_dir))
        # Check every existing ancestor for symlinks; state itself must be private.
        for p in (self.state, *self.state.parents):
            if p.is_symlink():
                raise Failure("unsafe_state_path")
        private_dir(self.state)
        if len(os.fsencode(self.state)) > 68:
            raise Failure("socket_path_too_long")
        lock = self.state / "manager.lock"
        fd = os.open(lock, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        self.lock = os.fdopen(fd, "r+")
        guard(lock)
        try:
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            self.lock.close()
            raise Failure("already_running") from None
        self.artifacts = self.state / "artifacts"
        private_dir(self.artifacts)
        self.checkout = self.state / "source"
        self.stopping = False
        self.building = False
        self.build_budget = MAX_WORKSPACE
        self.groups = set()
        self.current = None
        self.retired = []
        self.failures = {}
        self.restart_at = 0
        self.crashes = 0
        self.active_record = None
        try:
            data = read_json(self.state / "failures.json")
            if isinstance(data, dict):
                self.failures = {s: v for s, v in data.items() if SHA.fullmatch(s)
                                 and isinstance(v, dict) and isinstance(v.get("count"), int)
                                 and isinstance(v.get("after"), (int, float))}
        except (OSError, ValueError, Failure):
            pass

    def status(self, phase, revision=None, error=None):
        data = {"phase": phase}
        if revision and SHA.fullmatch(revision):
            data["revision"] = revision
        if error:
            data["errorCode"] = error
        atomic_json(self.state / "status.json", data)

    def stop_process(self, p):
        # A process group can outlive its leader, so never skip based on poll().
        if p not in self.groups:
            return
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            p.wait(timeout=2)
        except subprocess.TimeoutExpired:
            pass
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        p.wait()
        self.groups.discard(p)

    def spawn(self, argv, cwd=None, env=None, capture=False):
        p = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL, start_new_session=True)
        self.groups.add(p)
        return p

    def command(self, argv, cwd=None, env=None, timeout=3600, capture=False):
        # Captured commands are restricted to small fixed-output provenance/Git calls.
        p = self.spawn(argv, cwd, env, capture)
        deadline = time.monotonic() + timeout
        try:
            while p.poll() is None:
                self.maintenance()
                if self.stopping:
                    raise Failure("shutdown")
                if time.monotonic() > deadline:
                    raise Failure("command_timeout")
                time.sleep(0.05)
            if self.building:
                self.disk_reserve()
            output = p.stdout.read(4097) if capture else b""
            if p.returncode or len(output) > 4096:
                raise Failure("command_failed")
            return output.decode("utf-8").strip()
        finally:
            self.stop_process(p)
            if p.stdout:
                p.stdout.close()

    def fetch(self):
        if not self.checkout.exists():
            private_dir(self.checkout)
            self.command(["git", "init", "--quiet", str(self.checkout)], timeout=30)
        guard(self.checkout, directory=True)
        env = dict(os.environ, GIT_TERMINAL_PROMPT="0", GCM_INTERACTIVE="never")
        self.command(["git", "-c", "gc.auto=0", "fetch", "--quiet", "--depth=1",
                      "--no-tags", self.args.repository,
                      "+refs/heads/" + self.args.branch + ":refs/remotes/channel/tip"],
                     self.checkout, env, timeout=120)
        sha = self.command(["git", "rev-parse", "--verify", "refs/remotes/channel/tip^{commit}"],
                           self.checkout, env, timeout=10, capture=True)
        if not SHA.fullmatch(sha):
            raise Failure("invalid_revision")
        return sha

    def disk_reserve(self):
        free = shutil.disk_usage(self.state).free
        if free < MIN_FREE_BYTES:
            raise Failure("disk_reserve")
        return free

    def workspace_size(self):
        roots = (self.checkout, self.state / "cargo-cache", self.state / "npm-cache")
        return sum(p.stat(follow_symlinks=False).st_size for root in roots if root.exists()
                   for p in root.rglob("*") if not p.is_symlink() and p.is_file())

    def workspace_limit(self):
        self.disk_reserve()
        if self.workspace_size() > self.build_budget:
            raise Failure("workspace_limit")

    def build(self, sha):
        env = dict(os.environ, CARGO_BUILD_JOBS=str(self.args.build_jobs),
                   CARGO_INCREMENTAL="0", HERDR_BUILD_REVISION=sha)
        # Never inherit a cross-target or external target-directory override.
        env.pop("CARGO_BUILD_TARGET", None)
        env["CARGO_TARGET_DIR"] = str(self.checkout / "target")
        env["CARGO_HOME"] = str(self.state / "cargo-cache")
        env["npm_config_cache"] = str(self.state / "npm-cache")
        try:
            self.build_budget = min(MAX_WORKSPACE, self.workspace_size() + self.disk_reserve() - MIN_FREE_BYTES)
            self.building = True
            self.command(["git", "checkout", "--quiet", "--detach", "--force", sha], self.checkout, timeout=30)
            self.command(["git", "clean", "-ffdx", "--quiet"], self.checkout, timeout=30)
            for cmd, directory in [(["npm", "ci"], "frontend"),
                                   (["npm", "run", "build"], "frontend"),
                                   (["cargo", "build", "--release", "--locked"], ".")]:
                self.command(cmd, self.checkout / directory, env)
                self.workspace_limit()
            binary = self.checkout / "target/release/herdr-webapp"
            revision = self.command([str(binary), "--build-revision"], timeout=10, capture=True)
            if revision != sha:
                raise Failure("revision_mismatch")
            with (self.checkout / "Cargo.toml").open("rb") as f:
                version = tomllib.load(f)["package"]["version"]
            if not isinstance(version, str) or not SEMVER.fullmatch(version):
                raise Failure("invalid_version")
            self.publish_artifact(sha, binary, version)
        finally:
            self.building = False
            # Cleanup must work even with the free-space reserve exhausted.
            # No unbounded compiler/npm cache retained in the dedicated workspace.
            for rel in ("target", "frontend/node_modules", "frontend/dist"):
                path = self.checkout / rel
                if path.is_symlink():
                    path.unlink()
                elif path.exists():
                    shutil.rmtree(path)
            for cache in (self.state / "cargo-cache", self.state / "npm-cache"):
                if cache.is_symlink():
                    cache.unlink()
                elif cache.exists():
                    shutil.rmtree(cache)
            self.command(["git", "reflog", "expire", "--expire=now", "--all"], self.checkout, timeout=30)
            self.command(["git", "gc", "--prune=now", "--quiet"], self.checkout, timeout=120)

    def publish_artifact(self, sha, binary, version):
        target = native_target()
        if not binary.is_file() or binary.is_symlink() or not 0 < binary.stat().st_size <= MAX_BINARY:
            raise Failure("invalid_binary")
        temp = Path(tempfile.mkdtemp(prefix=".artifact-", dir=self.artifacts))
        try:
            catalog = temp / "catalog"
            private_dir(catalog)
            dest = catalog / sha
            private_dir(dest)
            dest /= target
            private_dir(dest)
            dest /= "herdr-webapp"
            shutil.copyfile(binary, dest)
            dest.chmod(0o700)
            with dest.open("rb") as f:
                os.fsync(f.fileno())
            manifest = {"schemaVersion": 2, "protocolVersion": 1, "version": version,
                        "revision": sha, "channel": "main", "assets": [
                            {"target": target, "url": f"/main/client/{sha}/{target}/herdr-webapp",
                             "size": dest.stat().st_size, "sha256": digest(dest)}]}
            atomic_json(catalog / "manifest.json", manifest)
            os.rename(temp, self.artifacts / sha)
        finally:
            if temp.exists():
                shutil.rmtree(temp)

    def artifact(self, sha):
        if not isinstance(sha, str) or not SHA.fullmatch(sha):
            raise Failure("invalid_revision")
        root = self.artifacts / sha
        guard(root, directory=True)
        catalog = root / "catalog"
        guard(catalog, directory=True)
        manifest = read_json(catalog / "manifest.json")
        if (manifest.get("schemaVersion") != 2 or manifest.get("protocolVersion") != 1
                or manifest.get("channel") != "main" or manifest.get("revision") != sha
                or not SEMVER.fullmatch(str(manifest.get("version", "")))
                or not isinstance(manifest.get("assets"), list)
                or len(manifest["assets"]) != 1
                or not isinstance(manifest["assets"][0], dict)):
            raise Failure("invalid_catalog")
        asset = manifest["assets"][0]
        target = native_target()
        if asset.get("target") != target or asset.get("url") != f"/main/client/{sha}/{target}/herdr-webapp":
            raise Failure("invalid_catalog")
        guard(catalog / sha, directory=True)
        guard(catalog / sha / target, directory=True)
        binary = catalog / sha / target / "herdr-webapp"
        s = guard(binary, executable=True)
        if not 0 < s.st_size <= MAX_BINARY or s.st_size != asset.get("size") or digest(binary) != asset.get("sha256"):
            raise Failure("invalid_artifact")
        if self.command([str(binary), "--build-revision"], timeout=10, capture=True) != sha:
            raise Failure("revision_mismatch")
        return binary, catalog, manifest

    def child(self, binary, catalog, sha, sock):
        argv = [str(binary), "--public-origin", self.args.public_origin, "serve-relay",
                "--channel-base-path", "/main/", "--channel-revision", sha,
                "--unix-socket", str(sock), "--release-dir", str(catalog)]
        for url in self.args.turn_url:
            argv += ["--turn-url", url]
        if self.args.turn_secret_file:
            argv += ["--turn-secret-file", self.args.turn_secret_file,
                     "--turn-ttl-seconds", str(self.args.turn_ttl_seconds)]
        if self.args.allow_http_loopback:
            argv += ["--allow-http-loopback"]
        allowed = {"PATH", "HOME", "USER", "LOGNAME", "LANG", "TZ", "TMPDIR",
                   "SSL_CERT_FILE", "SSL_CERT_DIR"}
        env = {k: v for k, v in os.environ.items() if k in allowed or k.startswith("LC_")}
        env["HERDR_MAIN_PARENT_PID"] = str(os.getpid())
        return self.spawn(argv, env=env)

    def probe(self, sock, sha, manifest):
        deadline = time.monotonic() + 20
        def get(url, limit=1024 * 1024):
            self.maintenance()
            if self.stopping or time.monotonic() > deadline:
                raise Failure("readiness_timeout")
            status, headers, body = request(sock, url, limit)
            if status != 200 or time.monotonic() > deadline:
                raise Failure("readiness_failed")
            return headers, body
        _, body = get("/main/healthz")
        health = json.loads(body)
        if health.get("ready") is not True or health.get("protocolVersion") != 1:
            raise Failure("health_mismatch")
        _, body = get("/main/api/mode")
        mode = json.loads(body)
        if any(mode.get(k) != v for k, v in {"mode": "relay", "channel": "main", "basePath": "/main/",
                                             "revision": sha, "protocolVersion": 1,
                                             "entrypointProtocolVersion": 1,
                                             "publicOrigin": self.args.public_origin,
                                             "version": manifest["version"]}.items()):
            raise Failure("mode_mismatch")
        for root_path in ("/", "/api/mode", "/client/manifest.json", "/relay/socket"):
            if request(sock, root_path)[0] != 404:
                raise Failure("root_path_exposed")
        _, html = get("/main/")
        if b"<html" not in html.lower():
            raise Failure("assets_missing")
        # Verify every root page script/stylesheet under its channel, not a stable fallback.
        links = re.findall(rb'(?:src|href)=["\']([^"\']+)["\']', html)
        assets = [x.decode("utf-8") for x in links if b"/assets/" in x]
        if not assets or len(assets) > 32:
            raise Failure("assets_missing")
        for url in assets:
            parsed = urllib.parse.urlsplit(url)
            resolved = urllib.parse.urljoin("/main/", url)
            if (parsed.scheme or parsed.netloc or parsed.query or parsed.fragment
                    or ".." in url or "\\" in url or "%" in url
                    or not resolved.startswith("/main/assets/")):
                raise Failure("asset_base_mismatch")
            get(resolved, 8 * 1024 * 1024)
        _, body = get("/main/client/manifest.json")
        if json.loads(body) != manifest:
            raise Failure("catalog_mismatch")
        asset = manifest["assets"][0]
        _, binary = get(asset["url"], MAX_BINARY)
        if len(binary) != asset["size"] or hashlib.sha256(binary).hexdigest() != asset["sha256"]:
            raise Failure("download_mismatch")
        key = base64.b64encode(os.urandom(16)).decode()
        headers = {"Connection": "Upgrade", "Upgrade": "websocket", "Sec-WebSocket-Version": "13",
                   "Sec-WebSocket-Key": key, "Origin": self.args.public_origin}
        code, returned, challenge = request(sock, "/main/relay/socket", headers=headers)
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        returned = {k.lower(): v for k, v in returned.items()}
        if (code != 101 or returned.get("sec-websocket-accept") != accept
                or returned.get("upgrade", "").lower() != "websocket"
                or "upgrade" not in [v.strip() for v in returned.get("connection", "").lower().split(",")]):
            raise Failure("websocket_unavailable")
        challenge = json.loads(challenge)
        if (not isinstance(challenge, dict) or challenge.get("type") != "challenge"
                or not isinstance(challenge.get("nonce"), str)
                or not 16 <= len(challenge["nonce"]) <= 256):
            raise Failure("websocket_challenge_invalid")
        duplicate = list(headers.items()) + [("Origin", self.args.public_origin)]
        if request(sock, "/main/relay/socket", headers=duplicate)[0] != 403:
            raise Failure("duplicate_origin_not_enforced")
        headers["Origin"] = "https://invalid-origin.example"
        if request(sock, "/main/relay/socket", headers=headers)[0] != 403:
            raise Failure("origin_not_enforced")

    def activate(self, sha):
        binary, catalog, manifest = self.artifact(sha)
        sock = self.state / ("s-" + os.urandom(8).hex() + ".sock")
        p = self.child(binary, catalog, sha, sock)
        try:
            deadline = time.monotonic() + 30
            while True:
                self.maintenance()
                if self.stopping or p.poll() is not None:
                    raise Failure("candidate_exited")
                try:
                    self.probe(sock, sha, manifest)
                    break
                except (OSError, ValueError, Failure, http.client.HTTPException):
                    if self.stopping:
                        raise Failure("shutdown") from None
                    if time.monotonic() >= deadline:
                        raise Failure("readiness_failed") from None
                    time.sleep(0.2)
            self.maintenance()
            if self.stopping or p.poll() is not None:
                raise Failure("candidate_exited")
            record = {"revision": sha, "socket": sock.name,
                      "catalog": str(catalog.relative_to(self.state)), "protocolVersion": 1}
            atomic_json(self.state / "active.json", record)
        except BaseException:
            self.stop_process(p)
            sock.unlink(missing_ok=True)
            raise
        if self.current:
            self.retired.append((time.monotonic() + 10, *self.current))
        self.current = (p, sock, sha)
        self.active_record = record
        self.restart_at = 0
        # Publication above is the commit point; housekeeping failure must never
        # misreport a successful route switch as a candidate activation failure.
        try:
            self.status("ready", sha)
            self.prune()
        except (OSError, Failure):
            pass

    def prune(self):
        protected = {x[3] for x in self.retired}
        if self.current:
            protected.add(self.current[2])
        roots = sorted((p for p in self.artifacts.iterdir() if SHA.fullmatch(p.name)),
                       key=lambda p: p.stat().st_mtime, reverse=True)
        keep = set(protected)
        for p in roots:
            if len(keep) < 3:
                keep.add(p.name)
        # Retired processes temporarily pin extra artifacts for <=10 seconds.
        for p in roots:
            if p.name not in keep:
                guard(p, directory=True)
                shutil.rmtree(p)

    def maintenance(self):
        if os.getppid() != self.entry_pid:
            self.stopping = True
            raise Failure("entry_exited")
        if self.building:
            # command() unwinds and signals only that command's owned group;
            # the currently serving child is deliberately left running.
            self.disk_reserve()
        now = time.monotonic()
        for entry in self.retired[:]:
            deadline, p, sock, _ = entry
            if now >= deadline:
                self.stop_process(p)
                sock.unlink(missing_ok=True)
                self.retired.remove(entry)
        if self.current and self.current[0].poll() is not None:
            self.stop_process(self.current[0])
            self.current[1].unlink(missing_ok=True)
            self.current = None
            self.crashes += 1
            self.restart_at = now + min(300, 2 ** min(self.crashes, 9))
            self.status("unavailable", error="child_exited")

    def boot(self):
        try:
            record = read_json(self.state / "active.json")
            sha = record["revision"]
            self.activate(sha)
        except (OSError, ValueError, KeyError, Failure):
            self.status("unavailable", error="no_verified_artifact")

    def update(self):
        self.status("checking", self.current[2] if self.current else None)
        sha = self.fetch()
        if self.current and sha == self.current[2]:
            self.status("ready", sha)
            return
        failed = self.failures.get(sha, {"count": 0, "after": 0})
        if failed["count"] >= 3 or time.time() < failed["after"]:
            self.status("ready" if self.current else "unavailable", sha, "revision_backoff")
            return
        try:
            self.status("building", sha)
            if not (self.artifacts / sha).exists():
                self.build(sha)
            self.activate(sha)
            self.failures.pop(sha, None)
        except (OSError, ValueError, Failure):
            count = failed["count"] + 1
            self.failures[sha] = {"count": count, "after": time.time() + min(3600, 60 * 2**count)}
            self.status("ready" if self.current else "unavailable", sha, "candidate_failed")
        # Bounded persistent failure ledger; newest observed tip is always fetched next.
        self.failures = dict(list(self.failures.items())[-128:])
        atomic_json(self.state / "failures.json", self.failures)
        return True  # Attempted work: fetch the newest branch tip immediately.

    def close(self):
        self.stopping = True
        for p in list(self.groups):
            self.stop_process(p)
        for sock in self.state.glob("s-*.sock"):
            if stat.S_ISSOCK(sock.lstat().st_mode) and sock.lstat().st_uid == os.geteuid():
                sock.unlink()
        # Keep active revision for offline boot, but never leave a routable stale socket.
        if self.active_record:
            record = dict(self.active_record, socket=None)
            atomic_json(self.state / "active.json", record)
        self.lock.close()

    def run(self):
        next_poll = 0
        failures = 0
        try:
            self.boot()
            while not self.stopping:
                self.maintenance()
                if not self.current and self.active_record and time.monotonic() >= self.restart_at:
                    try:
                        self.activate(self.active_record["revision"])
                    except (OSError, ValueError, Failure):
                        self.crashes += 1
                        self.restart_at = time.monotonic() + min(300, 2 ** min(self.crashes, 9))
                if time.monotonic() >= next_poll:
                    try:
                        attempted = self.update()
                        failures = 0
                    except (OSError, ValueError, Failure):
                        attempted = False
                        failures += 1
                        self.status("ready" if self.current else "unavailable", error="update_failed")
                    delay = 0 if attempted else min(3600, self.args.interval_seconds * 2 ** min(failures, 4))
                    next_poll = time.monotonic() + delay * random.uniform(0.9, 1.1)
                time.sleep(0.2)
        finally:
            self.close()


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--state-dir", required=True)
    p.add_argument("--repository", default="https://github.com/perapp/herdr-webapp.git")
    p.add_argument("--branch", default="main")
    p.add_argument("--public-origin", required=True)
    p.add_argument("--interval-seconds", type=int, default=300)
    p.add_argument("--build-jobs", type=int, default=2)
    p.add_argument("--turn-url", action="append", default=[])
    p.add_argument("--turn-secret-file")
    p.add_argument("--turn-ttl-seconds", type=int, default=300)
    p.add_argument("--allow-http-loopback", action="store_true")
    return p


def main():
    os.umask(0o077)
    try:
        a = parser().parse_args()
        validate_args(a)
        manager = Manager(a)
        def stop(_signal, _frame):
            manager.stopping = True
        signal.signal(signal.SIGTERM, stop)
        signal.signal(signal.SIGINT, stop)
        manager.run()
        return 0
    except Exception:
        # This is a private supervisor: even unexpected failures must not emit
        # traceback paths, credentials, subprocess arguments or source output.
        print("main-channel: supervisor_unavailable", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
