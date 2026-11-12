#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx>=0.28,<0.29", "websockets>=15,<16"]
# ///
"""Real isolated-core acceptance smoke. Never connects to the user's core.

Creates two disposable local cores, federates them with a local --via adapter,
launches the standalone binary OUTSIDE the checkout, and tests HTTP + raw WS.
"""
import argparse
import asyncio
import contextlib
import json
import os
from pathlib import Path
import shlex
import secrets
import shutil
import socket
import subprocess
import tempfile
import time
from urllib.parse import parse_qs, urlsplit

import httpx
from websockets.asyncio.client import connect


async def main(binary: Path, keep: bool, hold: bool):
    root = Path(tempfile.mkdtemp(prefix="herdr-web-smoke-"))
    os.chmod(root, 0o700)
    hc = shutil.which("hc")
    assert hc, "hc must be installed"
    base = {k: os.environ[k] for k in ["PATH", "USER", "LOGNAME", "LANG", "SHELL"] if k in os.environ}
    envs = {}
    for name in ["a", "b"]:
        env = base | {"HOME": str(root / name / "home"), "HC_SESSION": name, "HC_SOCKET": str(root / f"{name}.sock")}
        for var, folder in [("XDG_STATE_HOME", "state"), ("XDG_CONFIG_HOME", "config"), ("XDG_RUNTIME_DIR", "run"), ("XDG_DATA_HOME", "data")]:
            path = root / name / folder
            path.mkdir(parents=True, mode=0o700)
            env[var] = str(path)
        Path(env["HOME"]).mkdir(mode=0o700)
        envs[name] = env

    def command(name, *args, check=True):
        r = subprocess.run([hc, *args], env=envs[name], cwd=root, capture_output=True, text=True, timeout=60)
        if check and r.returncode:
            raise AssertionError(f"isolated {name}: hc {' '.join(args)}: {r.stderr}")
        return r

    def rpc(name, method, params):
        with socket.socket(socket.AF_UNIX) as s:
            s.connect(envs[name]["HC_SOCKET"])
            f = s.makefile("rw")
            for i, (m, p) in enumerate([("hello", {"generation": 1}), (method, params)]):
                f.write(json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": p}) + "\n")
                f.flush()
                result = json.loads(f.readline())
                assert "error" not in result, result
            return result["result"]

    server = None
    started = []
    try:
        for name in ["a", "b"]:
            command(name, "server", "start")
            started.append(name)
        adapter = root / "via-beta"
        exports = " ".join(f"{k}={shlex.quote(v)}" for k, v in envs["b"].items())
        adapter.write_text(f"#!/bin/sh\nexec env -i {exports} /bin/sh -c \"$1\"\n")
        adapter.chmod(0o700)
        command("a", "machine", "add", "--name", "beta", "--via", str(adapter), "--session", "b", "--temp")
        # Preserve opaque extensions while editing real machine documents.
        for name in ["a", "b"]:
            rpc(name, "doc.set", {"name": "herdr-app", "version": 0, "value": {"spaces": [], "extension": {"preserve": True}}})
        token = secrets.token_urlsafe(32)
        token_file = root / "token"
        token_file.write_text(token)
        token_file.chmod(0o600)
        server = subprocess.Popen([str(binary.resolve()), "--bind", "127.0.0.1", "--port", "0", "--core-socket", envs["a"]["HC_SOCKET"], "--token-file", str(token_file), "--no-dashboard"], cwd=root, env=base | {"HOME": str(root)}, stdout=subprocess.PIPE, stderr=open(root / "server.stderr", "w"), text=True)
        origin = None
        for _ in range(10):
            line = await asyncio.wait_for(asyncio.to_thread(server.stdout.readline), 15)
            if line.startswith("LAN URL: "):
                login_url = line.strip().removeprefix("LAN URL: ")
                parsed = urlsplit(login_url)
                assert parse_qs(parsed.fragment).get("token") == [token], "LAN URL must embed the access token"
                origin = f"{parsed.scheme}://{parsed.netloc}"
                break
        assert origin, "binary did not print a usable LAN URL"
        # Drain QR/stdout without exposing credentials in test output.
        async def drain():
            while await asyncio.to_thread(server.stdout.readline):
                pass
        drain_task = asyncio.create_task(drain())
        async with httpx.AsyncClient(base_url=origin, timeout=30) as client:
            assert (await client.get("/")).status_code == 200
            index = (await client.get("/")).text
            assert "/assets/" in index, "compiled frontend must be embedded"
            assert (await client.get("/api/state")).status_code == 401
            assert (await client.post("/api/login", headers={"Origin": "http://evil.example"}, json={"token": token})).status_code == 403
            assert (await client.post("/api/login", headers={"Origin": origin}, json={"token": "wrong"})).status_code == 401
            response = await client.post("/api/login", headers={"Origin": origin}, json={"token": token})
            assert response.status_code == 200
            assert "HttpOnly" in response.headers["set-cookie"] and "SameSite=Strict" in response.headers["set-cookie"]
            client.headers["Origin"] = origin
            assert (await client.get("/api/session")).status_code == 200

            async def wait_state(predicate):
                async with asyncio.timeout(35):
                    async with client.stream("GET", "/api/events") as response:
                        assert response.status_code == 200
                        async for line in response.aiter_lines():
                            if line.startswith("data:"):
                                state = json.loads(line.removeprefix("data:").strip())
                                if predicate(state):
                                    return state
                raise AssertionError("No matching core event state")

            state = await wait_state(lambda s: s["connected"] and len(s["documents"]) == 2)
            print("PASS embedded assets, auth, cookie, CSRF, local + federated discovery")
            created = []
            for on in [None, "beta"]:
                doc = next(d for d in state["documents"] if d["on"] == on)
                r = await client.post("/api/topology", json={"action": "new-space", "name": "Local smoke" if on is None else "Federated smoke", "folder": envs["a" if on is None else "b"]["HOME"], "on": on, "version": doc["version"]})
                assert r.status_code == 200, r.text
                result = r.json()
                assert result["value"]["extension"]["preserve"] is True
                created.append(result)
            local, remote = created
            state = await wait_state(lambda s: any(t["address"] == remote["terminal"] for t in s["terminals"]))
            assert remote["terminal"].startswith("beta/")
            assert {t["address"] for t in state["terminals"]} >= {local["terminal"], remote["terminal"]}
            conflict = await client.post("/api/topology", json={"action": "rename-space", "space": local["space"], "name": "stale", "on": None, "version": 0})
            assert conflict.status_code == 409
            renamed = await client.post("/api/topology", json={"action": "rename-tab", "space": local["space"], "tab": local["tab"], "name": "Keyboard smoke", "on": None, "version": local["version"]})
            assert renamed.status_code == 200, renamed.text
            split = await client.post("/api/topology", json={"action": "split", "space": local["space"], "tab": local["tab"], "pane": local["pane"], "direction": "down", "on": None, "version": renamed.json()["version"]})
            assert split.status_code == 200, split.text
            print("PASS shared local/remote topology, opaque extension preservation, rename, split, stale CAS rejection")
            cookies = "; ".join(f"{k}={v}" for k, v in client.cookies.items())
            wsbase = origin.replace("http://", "ws://") + "/api/terminal"

            @contextlib.asynccontextmanager
            async def attach(address, observe=None, resumed=False):
                from urllib.parse import urlencode
                params = {"terminal": address, "cols": 91, "rows": 27, "resumed": str(resumed).lower()}
                if observe is not None:
                    params["observe"] = str(observe).lower()
                uri = wsbase + "?" + urlencode(params)
                async with connect(uri, origin=origin, additional_headers={"Cookie": cookies}, max_size=20 * 1024 * 1024) as ws:
                    async with asyncio.timeout(15):
                        while True:
                            message = await ws.recv()
                            if isinstance(message, str) and json.loads(message)["type"] == "attached":
                                assert json.loads(message)["observe"] is (observe is True), "default attachment must be interactive"
                                break
                    await ws.send(json.dumps({"type": "credit", "bytes": 262144}))
                    yield ws

            async def output_until(ws, needle):
                output = b""
                async with asyncio.timeout(20):
                    while needle not in output:
                        message = await ws.recv()
                        if isinstance(message, bytes):
                            output += message
                            await ws.send(json.dumps({"type": "credit", "bytes": len(message)}))
                        else:
                            data = json.loads(message)
                            assert data["type"] not in ["error", "closed"], data
                return output

            for item in created:
                async with attach(item["terminal"]) as ws:
                    await ws.send(json.dumps({"type": "input", "data": "printf '\\x57\\x45\\x42\\x5f\\x52\\x4f\\x55\\x4e\\x44\\x54\\x52\\x49\\x50\\x5f\\x4f\\x4b\\n'\r"}))
                    await output_until(ws, b"WEB_ROUNDTRIP_OK")
                    await ws.send(json.dumps({"type": "resize", "cols": 103, "rows": 31}))
                    await ws.send(json.dumps({"type": "input", "data": "stty size\r"}))
                    await output_until(ws, b"31 103")
                async with attach(item["terminal"], observe=True, resumed=True) as ws:
                    await output_until(ws, b"WEB_ROUNDTRIP_OK")
            print("PASS local/federated raw terminal input/output, real PTY resize, observer reconnect first paint")
            before_observe = rpc("a","terminal.get",{"terminal":local["terminal"]})
            async with attach(local["terminal"], observe=True) as ws:
                await ws.send(json.dumps({"type":"resize","cols":9,"rows":9}))
                await ws.send(json.dumps({"type":"input","data":"printf OBSERVER_MUST_NOT_TYPE\r"}))
                await ws.send(json.dumps({"type":"focus","focused":True}))
                pong = await ws.ping()
                await pong
                facts = rpc("a","terminal.get",{"terminal":local["terminal"]})
                assert (facts["cols"],facts["rows"]) == (before_observe["cols"],before_observe["rows"]), facts
            async with attach(local["terminal"]) as ws:
                await ws.send(json.dumps({"type":"input","data":"head -c 600000 /dev/zero | tr '\\0' X; printf '\\nFLOW_CONTROL_OK\\n'\r"}))
                await output_until(ws,b"FLOW_CONTROL_OK\r\n")
            from websockets.exceptions import InvalidStatus
            for bad_origin, bad_cookies, expected in [("http://evil.example",cookies,403),(origin,"",401)]:
                try:
                    async with connect(wsbase+"?terminal="+local["terminal"],origin=bad_origin,additional_headers={"Cookie":bad_cookies}):
                        raise AssertionError("Unauthenticated/foreign-origin WebSocket was accepted")
                except InvalidStatus as exc:
                    assert exc.response.status_code == expected
            print("PASS observer input/resize isolation, output beyond credit window, WebSocket auth/origin checks")
            # Status reports flow via events rather than reading screenshots or text.
            rpc("b", "terminal.send", {"terminal": remote["terminal"].split("/", 1)[1], "text": "printf '\\033]7501;state=working:app=smoke\\033\\\\'"})
            await wait_state(lambda s: any(t["address"] == remote["terminal"] and (t.get("status") or {}).get("state") == "working" for t in s["terminals"]))
            print("PASS federated OSC status events")
            # Restart ONLY isolated A, prove disconnected then fresh subscribe snapshot.
            command("a", "server", "stop")
            await wait_state(lambda s: not s["connected"])
            command("a", "server", "start")
            await wait_state(lambda s: s["connected"] and any(t["address"]==remote["terminal"] for t in s["terminals"]))
            print("PASS isolated core disconnect and fresh-snapshot reconnect")
            async with attach(remote["terminal"],observe=True) as ws:
                assert (await client.post("/api/logout")).status_code == 200
                async with asyncio.timeout(12):
                    try:
                        while True:
                            message=await ws.recv()
                            if isinstance(message,str) and json.loads(message).get("type")=="error":
                                break
                    except Exception as exc:
                        from websockets.exceptions import ConnectionClosed
                        assert isinstance(exc,ConnectionClosed),exc
            assert (await client.get("/api/state")).status_code == 401
            print("PASS logout revokes session")
        print("ALL SMOKE CHECKS PASSED (standalone binary launched outside checkout)", flush=True)
        if hold:
            print(f"VISUAL_FIXTURE_ROOT={root}", flush=True)
            print(f"VISUAL_FIXTURE_URL={origin}", flush=True)
            print("Isolated visual fixture is running. Ctrl+C cleans up only these test cores.", flush=True)
            await asyncio.Event().wait()
        if keep:
            print(f"Kept isolated test artifacts: {root}")
    finally:
        if server:
            server.terminate()
            try:
                await asyncio.to_thread(server.wait, 10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
        # Remove the owned federation first: otherwise A can auto-start B again
        # while cleanup stops it, recreating its scratch directory after removal.
        if "a" in started:
            command("a", "machine", "rm", "beta", check=False)
        for name in started:
            command(name, "server", "stop", check=False)
        if not keep:
            shutil.rmtree(root)
        else:
            print(f"Isolated artifacts: {root}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/herdr-webapp"))
    parser.add_argument("--keep", action="store_true")
    parser.add_argument("--hold", action="store_true", help="Keep the isolated fixture running after tests for browser verification; Ctrl+C cleans up")
    args = parser.parse_args()
    try:
        asyncio.run(main(args.binary, args.keep, args.hold))
    except KeyboardInterrupt:
        pass
