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
        async def launch_server(port=0):
            process = subprocess.Popen([str(binary.resolve()), "--bind", "127.0.0.1", "--port", str(port), "--core-socket", envs["a"]["HC_SOCKET"], "--token-file", str(token_file), "--no-dashboard"], cwd=root, env=base | {"HOME": str(root)}, stdout=subprocess.PIPE, stderr=open(root / "server.stderr", "a"), text=True)
            # Record immediately so failure cleanup owns this process too.
            nonlocal server
            server = process
            origin = None
            for _ in range(10):
                line = await asyncio.wait_for(asyncio.to_thread(process.stdout.readline), 15)
                if line.startswith("LAN URL: "):
                    parsed = urlsplit(line.strip().removeprefix("LAN URL: "))
                    assert parse_qs(parsed.fragment).get("token") == [token], "LAN URL must embed the access token"
                    origin = f"{parsed.scheme}://{parsed.netloc}"
                    break
            assert origin, "binary did not print a usable LAN URL"
            # Drain QR/stdout without exposing credentials in test output.
            async def drain():
                while await asyncio.to_thread(process.stdout.readline):
                    pass
            return origin, asyncio.create_task(drain())

        origin, drain_task = await launch_server()

        async def restart_server():
            nonlocal drain_task
            server.terminate()
            await asyncio.to_thread(server.wait, 10)
            await drain_task
            restarted_origin, drain_task = await launch_server(urlsplit(origin).port)
            assert restarted_origin == origin

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
            session_before_restart = client.cookies.get("herdr_session")
            await restart_server()
            assert (await client.get("/api/session")).status_code == 200, "browser session must survive server restart"
            assert client.cookies.get("herdr_session") == session_before_restart
            assert (await client.post("/api/logout", headers={"Origin": "http://evil.example"})).status_code == 403
            print("PASS browser session survives real web-server restart; Origin protections unchanged")
            update = await client.get("/api/updates")
            assert update.status_code == 200, update.text
            update = update.json()
            assert isinstance(update.get("current_version"), str) and update["current_version"]
            assert update.get("source") == "development" and update.get("can_install") is False, "standalone source binary must not replace itself"
            async with client.stream("GET", "/api/updates/events") as response:
                assert response.status_code == 200
                async for line in response.aiter_lines():
                    if line.startswith("data:"):
                        event = json.loads(line.removeprefix("data:").strip())
                        assert event["current_version"] == update["current_version"]
                        break
            for action in ["check", "install"]:
                r = await client.post(f"/api/updates/{action}", headers={"Origin": "http://evil.example"}, json={} if action == "check" else {"version": "99.0.0"})
                assert r.status_code == 403
            r = await client.post("/api/updates/install", json={"version": "99.0.0"})
            assert r.status_code in [400, 409], "source build update must be refused"
            async with httpx.AsyncClient(base_url=origin) as anonymous:
                for path in ["/api/updates", "/api/updates/events"]:
                    assert (await anonymous.get(path)).status_code == 401
                assert (await anonymous.post("/api/updates/install", json={"version": "99.0.0"})).status_code == 401
            print("PASS update status/events auth, exact Origin, and source-build replacement refusal")

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
            for item in created:
                current = rpc("b" if item["on"] else "a", "doc.get", {"name":"herdr-app"})
                new_tab = await client.post("/api/topology", json={"action":"new-tab","on":item["on"],"version":current["version"],"space":item["space"],"name":"Pane creation smoke"})
                assert new_tab.status_code == 200, new_tab.text
                target = new_tab.json()
                before = target["value"]["spaces"][0]["tabs"][-1]["root"]["children"]
                payload = {"action":"new-pane","on":item["on"],"version":target["version"],"space":item["space"],"tab":target["tab"]}
                added = await client.post("/api/topology", json=payload)
                assert added.status_code == 200, added.text
                result = added.json()
                children = result["value"]["spaces"][0]["tabs"][-1]["root"]["children"]
                assert children[:-1] == before
                assert children[-1]["id"] == result["pane"]
                assert children[-1]["content"]["terminal"] == result["terminal"].split("/")[-1]
                assert result["value"]["extension"]["preserve"] is True
                assert (await client.post("/api/topology", json=payload)).status_code == 409
            print("PASS local/federated new-pane creation, existing-node preservation and stale creation rejection")
            # Catalog transport is native per-machine doc RPC, never local sidecar fallback.
            # Synthetic Pi status binds test catalogs to these disposable shell processes;
            # a separate real-Pi browser acceptance checks the actual extension publisher.
            for item in created:
                machine = "b" if item["on"] else "a"
                terminal_id = item["terminal"].split("/")[-1]
                rpc(machine, "terminal.send", {"terminal": terminal_id, "text": "printf '\\033]7501;state=idle:app=pi\\033\\\\'"})
                await wait_state(lambda s: any(t["address"] == item["terminal"] and (t.get("status") or {}).get("app") == "pi" for t in s["terminals"]))
                identity = rpc(machine, "terminal.get", {"terminal": terminal_id})
                name = f"herdr-pi-commands-{terminal_id}"
                catalog = {
                    "schemaVersion": 1, "terminal": terminal_id,
                    "terminalCreatedMs": identity["created_ms"], "terminalPid": identity["pid"],
                    "pid": (identity.get("foreground") or {}).get("pid", identity["pid"]),
                    "sessionId": "isolated-command-smoke", "updatedAt": int(time.time() * 1000),
                    "commands": [{"name": "local-only" if machine == "a" else "remote-only", "description": "Fixture catalog", "source": "extension"},
                                 {"name": "git-mode", "description": "A harmless fixture, not the real mode toggle", "source": "extension"}],
                }
                def store(value):
                    doc = rpc(machine, "doc.get", {"name": name})
                    rpc(machine, "doc.set", {"name": name, "version": doc["version"], "value": value})
                store(catalog)
                query = {"terminal": item["terminal"]}
                response = await client.get("/api/commands", params=query)
                assert response.status_code == 200 and response.json()["available"], response.text
                assert response.json()["commands"] == catalog["commands"], "catalog came from the wrong core"
                for changes in [{"updatedAt": int(time.time() * 1000) - 76_000}, {"updatedAt": int(time.time() * 1000) + 10_000},
                                {"terminalCreatedMs": 0}, {"pid": 0}, {"terminal": "twrong"}]:
                    store(catalog | changes)
                    response = await client.get("/api/commands", params=query)
                    assert response.status_code == 200 and not response.json()["available"]
                    assert response.json()["commands"] == []
                store(catalog)
                no_origin = client.build_request("GET", "/api/commands", params=query)
                del no_origin.headers["Origin"]
                assert (await client.send(no_origin)).json()["available"], "authenticated GET needs no Origin"
                assert (await client.get("/api/commands", params=query, headers={"Origin": "http://evil.example"})).status_code == 403
                assert (await client.get("/api/commands", params=query | {"path": "/secret"})).status_code == 400
                assert (await client.get("/api/commands", params=query | {"on": "untrusted"})).status_code == 400
            async with httpx.AsyncClient(base_url=origin) as anonymous:
                assert (await anonymous.get("/api/commands", params={"terminal": local["terminal"]})).status_code == 401
            print("PASS native local/federated command catalogs, exact destination, stale/run rejection, auth and Origin")
            # Upload uses native terminal.put on the selected terminal's machine.
            # Nothing is typed or submitted merely by storing an image.
            import base64
            image = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGMwXpUGAAJXAUQvQxIzAAAAAElFTkSuQmCC")
            upload_headers = {"Content-Type": "application/octet-stream"}
            for item in created:
                uploaded = await client.post("/api/attachments", params={"terminal": item["terminal"]}, content=image, headers=upload_headers)
                assert uploaded.status_code == 200, uploaded.text
                result = uploaded.json()
                path = Path(result["path"])
                assert path.is_relative_to(root / ("b" if item["on"] else "a")), "upload went to the wrong machine"
                assert path.suffix == ".png" and path.read_bytes() == image
                assert result["terminal"] == item["terminal"] and result["bytes"] == len(image)
            assert (await client.post("/api/attachments", params={"terminal": local["terminal"]}, content=image, headers=upload_headers | {"Origin": "http://evil.example"})).status_code == 403
            async with httpx.AsyncClient(base_url=origin) as anonymous:
                assert (await anonymous.post("/api/attachments", params={"terminal": local["terminal"]}, content=image, headers=upload_headers | {"Origin": origin})).status_code == 401
            assert (await client.post("/api/attachments", params={"terminal": local["terminal"]}, content=b"not an image", headers=upload_headers)).status_code == 415
            # Oversized declared bodies must be rejected before upload begins.
            # Send headers only: clients sending an entire forbidden body can
            # otherwise see a TCP reset instead of the server's early 413.
            parsed_origin = urlsplit(origin)
            reader, writer = await asyncio.open_connection(parsed_origin.hostname, parsed_origin.port)
            upload_cookies = "; ".join(f"{k}={v}" for k, v in client.cookies.items())
            writer.write((f"POST /api/attachments?terminal={local['terminal']} HTTP/1.1\r\nHost: {parsed_origin.netloc}\r\nOrigin: {origin}\r\nCookie: {upload_cookies}\r\nContent-Type: application/octet-stream\r\nContent-Length: {8 * 1024 * 1024 + 1}\r\nConnection: close\r\n\r\n").encode())
            await writer.drain()
            assert b" 413 " in await asyncio.wait_for(reader.readline(), 5)
            writer.close()
            await writer.wait_closed()
            # A valid PNG above the normal 1 MiB API body cap exercises this route's
            # bounded binary upload path rather than weakening all API limits.
            import struct
            import zlib
            def png_chunk(kind, data):
                return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
            pixels = b"".join(b"\x00" + os.urandom(600 * 3) for _ in range(600))
            large = b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", struct.pack(">IIBBBBB", 600, 600, 8, 2, 0, 0, 0)) + png_chunk(b"IDAT", zlib.compress(pixels)) + png_chunk(b"IEND", b"")
            assert len(large) > 1024 * 1024
            uploaded = await client.post("/api/attachments", params={"terminal": local["terminal"]}, content=large, headers=upload_headers)
            assert uploaded.status_code == 200, uploaded.text
            assert Path(uploaded.json()["path"]).read_bytes() == large
            print("PASS bounded native image upload, exact local/federated destination, auth/Origin and format/size rejection")
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
                    # Live theme updates use native viewer FACTS, not reconnect or input.
                    await ws.send(json.dumps({"type": "theme", "theme": "light"}))
                    await ws.send(json.dumps({"type": "theme", "theme": "dark"}))
                    await ws.send(json.dumps({"type": "resize", "cols": 103, "rows": 31}))
                    await ws.send(json.dumps({"type": "input", "data": "stty size\r"}))
                    await output_until(ws, b"31 103")
                async with attach(item["terminal"], observe=True, resumed=True) as ws:
                    await output_until(ws, b"WEB_ROUNDTRIP_OK")
            print("PASS local/federated input/output, live theme FACTS, real PTY resize, observer reconnect first paint")
            before_observe = rpc("a","terminal.get",{"terminal":local["terminal"]})
            async with attach(local["terminal"], observe=True) as ws:
                await ws.send(json.dumps({"type":"resize","cols":9,"rows":9}))
                await ws.send(json.dumps({"type":"input","data":"printf OBSERVER_MUST_NOT_TYPE\r"}))
                await ws.send(json.dumps({"type":"focus","focused":True}))
                await ws.send(json.dumps({"type":"theme","theme":"light"}))
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
            await restart_server()
            assert (await client.get("/api/session", headers={"Cookie": cookies})).status_code == 401, "logged-out session must remain revoked after restart"
            try:
                async with connect(wsbase+"?terminal="+local["terminal"], origin=origin, additional_headers={"Cookie": cookies}):
                    raise AssertionError("Revoked session regained WebSocket access after restart")
            except InvalidStatus as exc:
                assert exc.response.status_code == 401
            print("PASS logout revokes HTTP/WebSocket session across real web-server restart")
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
