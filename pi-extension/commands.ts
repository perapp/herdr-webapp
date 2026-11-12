import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { createConnection, type Socket } from "node:net";
import { isDeepStrictEqual } from "node:util";
import { existsSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const MAX_BYTES = 128 * 1024;
const NAME = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$/;
const LOCAL = /^t[0-9a-z]{1,13}$/;
const UNSAFE = /[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/gu;
type Command = { name: string; description: string; source: "extension" | "template" | "skill" };
type Terminal = { address: string; state: string; created_ms: number; pid: number; foreground?: { pid: number }; status?: { app?: string } };
type Catalog = { schemaVersion: 1; terminal: string; terminalCreatedMs: number; terminalPid: number; pid: number; sessionId: string; updatedAt: number; commands: Command[] };
type Doc = { version: number; value: unknown };

/** Native Unix discovery, matching herdr-core's platform/unix.rs and web config.rs.
 * No TOML parser dependency: custom config requires HC_SOCKET explicitly. */
function socketPath(): string {
  if (process.platform !== "linux" && process.platform !== "darwin") throw new Error("Unix command discovery is unsupported on this platform");
  if (process.env.HC_SOCKET) return process.env.HC_SOCKET;
  const config = join(process.env.XDG_CONFIG_HOME || join(homedir(), ".config"), "herdr-core/config.toml");
  if (existsSync(config)) throw new Error("Command discovery requires HC_SOCKET when herdr-core/config.toml exists");
  const session = process.env.HC_SESSION?.trim() || "default";
  if (session === "." || session === ".." || /[\p{Cc}/\\:*?"<>|]/u.test(session)) throw new Error("Invalid native session");
  let runtime = process.env.XDG_RUNTIME_DIR;
  if (!runtime && typeof process.getuid === "function") {
    const candidate = `/run/user/${process.getuid()}`;
    try { if (statSync(candidate).isDirectory()) runtime = candidate; } catch { /* Native fallback below. */ }
  }
  const base = runtime ? join(runtime, "herdr-core") : join(process.env.XDG_STATE_HOME || join(homedir(), ".local/state"), "herdr-core", session);
  return join(base, `${session}.sock`);
}

/** One bounded in-flight JSON-line native RPC, reused until disconnected. */
class Rpc {
  private socket?: Socket;
  private buffer = Buffer.alloc(0);
  private next = 0;
  private pending?: { id: number; resolve: (value: any) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> };
  private ready = false;
  constructor(private path: string) {}
  close() {
    const pending = this.pending;
    this.pending = undefined;
    if (pending) { clearTimeout(pending.timer); pending.reject(new Error("Native RPC disconnected")); }
    this.ready = false;
    this.socket?.destroy();
    this.socket = undefined;
    this.buffer = Buffer.alloc(0);
  }
  private request(method: string, params: object): Promise<any> {
    if (this.pending || !this.socket) return Promise.reject(new Error("Native RPC busy or disconnected"));
    return new Promise((resolve, reject) => {
      const id = ++this.next;
      const timer = setTimeout(() => this.close(), 2000);
      timer.unref();
      this.pending = { id, resolve, reject, timer };
      this.socket!.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n", error => { if (error) this.close(); });
    });
  }
  async call(method: string, params: object): Promise<any> {
    if (!this.socket) {
      const socket = createConnection({ path: this.path });
      this.socket = socket;
      socket.unref();
      socket.on("error", () => { if (this.socket === socket) this.close(); });
      socket.on("close", () => { if (this.socket === socket) this.close(); });
      socket.on("data", chunk => {
        if (this.socket !== socket) return;
        this.buffer = Buffer.concat([this.buffer, chunk]);
        if (this.buffer.length > 256 * 1024) { this.close(); return; }
        let end: number;
        while ((end = this.buffer.indexOf(10)) >= 0) {
          const line = this.buffer.subarray(0, end);
          this.buffer = this.buffer.subarray(end + 1);
          try {
            const message = JSON.parse(line.toString("utf8"));
            const pending = this.pending;
            if (!pending || message.id !== pending.id) continue;
            this.pending = undefined;
            clearTimeout(pending.timer);
            if (message.error || !("result" in message)) pending.reject(new Error("Native RPC rejected request"));
            else pending.resolve(message.result);
          } catch { this.close(); return; }
        }
      });
      const hello = await this.request("hello", { generation: 1 });
      if (hello?.generation !== 1) { this.close(); throw new Error("Unsupported native generation"); }
      this.ready = true;
    }
    if (!this.ready) throw new Error("Native RPC is not ready");
    return this.request(method, params);
  }
}

function validTerminal(value: any, terminal: string): value is Terminal {
  return value?.address === terminal && value.state === "live" && value.status?.app === "pi"
    && Number.isSafeInteger(value.created_ms) && value.created_ms > 0 && Number.isSafeInteger(value.pid) && value.pid > 0
    && (value.foreground ? value.foreground.pid === process.pid : value.pid === process.pid);
}
function sameRun(a: Terminal, b: Terminal): boolean {
  return a.address === b.address && a.created_ms === b.created_ms && a.pid === b.pid
    && (a.foreground?.pid ?? a.pid) === (b.foreground?.pid ?? b.pid);
}
function validDoc(doc: any): doc is Doc { return Number.isSafeInteger(doc?.version) && doc.version >= 0 && "value" in doc; }
function description(value: unknown): string {
  if (typeof value !== "string") return "";
  let result = "";
  for (const character of value.replace(UNSAFE, " ")) {
    // The browser's 200-character guard counts UTF-16 units; never split a surrogate pair.
    if (result.length + character.length > 200) break;
    result += character;
  }
  return result;
}

/** Only Pi's registered command API is enumerated; no commands execute and no session/source files are read. */
export default function (pi: ExtensionAPI) {
  let context: ExtensionContext | undefined;
  let rpc: Rpc | undefined;
  let interval: ReturnType<typeof setInterval> | undefined;
  let deferred: ReturnType<typeof setTimeout> | undefined;
  let flight: Promise<void> | undefined;
  let stopped = true;
  let retryAt = 0;
  let backoff = 1000;
  let owned: { version: number; value: Catalog } | undefined;
  let terminal = "";
  let name = "";

  async function publish() {
    if (stopped || !context || !rpc || Date.now() < retryAt) return;
    try {
      const current = await rpc.call("terminal.get", { terminal });
      if (stopped || !validTerminal(current, terminal)) return;
      const sessionId = context.sessionManager.getSessionId();
      if (typeof sessionId !== "string" || !sessionId || sessionId.length > 256 || /[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/u.test(sessionId)) return;
      const value: Catalog = { schemaVersion: 1, terminal, terminalCreatedMs: current.created_ms, terminalPid: current.pid,
        pid: process.pid, sessionId, updatedAt: Date.now(), commands: [] };
      const seen = new Set<string>();
      let bytes = Buffer.byteLength(JSON.stringify(value));
      for (const entry of pi.getCommands()) {
        if (typeof entry.name !== "string" || !NAME.test(entry.name) || seen.has(entry.name)) continue;
        const source = entry.source === "prompt" ? "template" : entry.source;
        if (source !== "extension" && source !== "template" && source !== "skill") continue;
        const command: Command = { name: entry.name, source,
          description: description(entry.description) };
        const cost = Buffer.byteLength(JSON.stringify(command)) + (value.commands.length ? 1 : 0);
        if (bytes + cost > MAX_BYTES || value.commands.length >= 500) break;
        value.commands.push(command); seen.add(entry.name); bytes += cost;
      }
      if (Buffer.byteLength(JSON.stringify(value)) > MAX_BYTES) return;
      const doc = await rpc.call("doc.get", { name });
      if (!validDoc(doc) || stopped) return;
      // Recheck after the asynchronous document read, before CAS publication.
      const fresh = await rpc.call("terminal.get", { terminal });
      if (stopped || !validTerminal(fresh, terminal) || !sameRun(current, fresh)) return;
      // Never blindly retry a mutation. The next heartbeat starts with a fresh doc.get.
      owned = undefined;
      const result = await rpc.call("doc.set", { name, value, version: doc.version });
      if (Number.isSafeInteger(result?.version) && result.version === doc.version + 1) owned = { version: result.version, value };
      retryAt = 0; backoff = 1000;
    } catch {
      rpc?.close(); retryAt = Date.now() + backoff; backoff = Math.min(backoff * 2, 30_000);
      // Metadata failures must never break Pi or disclose paths/core errors.
    }
  }
  function schedule() {
    if (!flight && !stopped) flight = publish().finally(() => { flight = undefined; });
  }
  async function stop() {
    stopped = true;
    clearInterval(interval); clearTimeout(deferred); interval = undefined; deferred = undefined;
    await flight;
    const previous = owned;
    owned = undefined;
    try {
      if (previous && rpc) {
        const doc = await rpc.call("doc.get", { name });
        // Version AND complete publisher value distinguish reloads and newer publishers,
        // even those with the same process/session. CAS protects the final read/write gap.
        if (validDoc(doc) && doc.version === previous.version && isDeepStrictEqual(doc.value, previous.value)) {
          await rpc.call("doc.set", { name, value: null, version: doc.version });
        }
      }
    } catch { /* TTL expires crashes, ambiguous writes, and unavailable cores. */ }
    finally { rpc?.close(); rpc = undefined; context = undefined; }
  }
  async function start(_event: unknown, current: ExtensionContext) {
    await stop();
    if (["0", "false", "no", "off"].includes((process.env.HERDR_COMMAND_DISCOVERY ?? "1").trim().toLowerCase())) return;
    terminal = process.env.HC_TERMINAL_ID ?? "";
    name = `herdr-pi-commands-${terminal}`;
    if (!LOCAL.test(terminal) || name.length > 32) return;
    try { rpc = new Rpc(socketPath()); }
    catch {
      if (current.hasUI) current.ui.notify("Live command discovery needs HC_SOCKET for custom core config (or a supported Unix platform).", "warning");
      return;
    }
    context = current; stopped = false; retryAt = 0; backoff = 1000;
    // Other extensions can register in session_start/resources_discover; defer the first snapshot.
    deferred = setTimeout(schedule, 0); deferred.unref();
    interval = setInterval(schedule, 15_000); interval.unref();
  }
  pi.on("session_start", start);
  // Older Pi versions emit session_switch; current Pi replaces runtimes and emits session_start.
  (pi.on as (event: string, handler: typeof start) => unknown)("session_switch", start);
  pi.on("session_shutdown", stop);
}
