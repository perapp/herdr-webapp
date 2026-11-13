import { FramedChannel } from './peerFrames';
import type { ApplicationTransport, EventSourceLike, SocketLike } from './transport';

function base64(bytes: Uint8Array): string {
  let text = ''; for (let i = 0; i < bytes.length; i += 8192) text += String.fromCharCode(...bytes.subarray(i, i + 8192));
  return btoa(text);
}
function decode(value: unknown): Uint8Array {
  if (typeof value !== 'string' || value.length > 24 * 1024 * 1024 || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) throw new Error('Invalid peer response');
  return Uint8Array.from(atob(value), c => c.charCodeAt(0));
}
function response(status: unknown, body: unknown): Response {
  if (!Number.isInteger(status) || Number(status) < 200 || Number(status) > 599) throw new Error('Invalid peer response status');
  const bytes = decode(body);
  return new Response(Number(status) === 204 || Number(status) === 205 || Number(status) === 304 ? null : bytes, { status: Number(status), headers: { 'Content-Type': 'application/json' } });
}
class PeerEvents implements EventSourceLike {
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  private listeners = new Map<string, ((event: MessageEvent) => void)[]>();
  constructor(readonly cancel: () => void) {}
  addEventListener(type: string, listener: (event: MessageEvent) => void): void { this.listeners.set(type, [...(this.listeners.get(type) || []), listener]); }
  event(type: string, data: unknown): void { for (const listener of this.listeners.get(type) || []) listener(new MessageEvent(type, { data })); }
  close(): void { this.cancel(); this.listeners.clear(); }
}
class PeerSocket implements SocketLike {
  readyState = 0;
  binaryType: BinaryType = 'arraybuffer';
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  private wire: FramedChannel;
  constructor(private channel: RTCDataChannel, attach: object, private disposed: () => void) {
    this.wire = new FramedChannel(channel);
    this.wire.onmessage = data => { if (this.readyState === 1) this.onmessage?.(new MessageEvent('message', { data })); };
    this.wire.onerror = () => { this.onerror?.(new Event('error')); this.close(); };
    channel.addEventListener('close', () => this.close());
    channel.addEventListener('open', () => {
      void this.wire.send(JSON.stringify(attach)).then(() => {
        if (this.readyState !== 0) return;
        this.readyState = 1; this.onopen?.(new Event('open'));
      }).catch(() => this.close());
    });
  }
  send(data: string | ArrayBufferLike | Blob | ArrayBufferView): void {
    if (this.readyState !== 1) throw new DOMException('Terminal is disconnected', 'InvalidStateError');
    if (typeof data !== 'string' && !(data instanceof ArrayBuffer) && !ArrayBuffer.isView(data)) throw new Error('Unsupported terminal data');
    const value = typeof data === 'string' || data instanceof ArrayBuffer ? data : new Uint8Array(data.buffer as ArrayBuffer, data.byteOffset, data.byteLength);
    void this.wire.send(value).catch(() => this.close());
  }
  close(): void {
    if (this.readyState === 3) return;
    this.readyState = 3; this.wire.close(); this.channel.close(); this.disposed();
    this.onclose?.(new CloseEvent('close', { code: 1006, reason: 'Peer channel closed' }));
  }
}
interface Pending { resolve: (response: Response) => void; reject: (error: Error) => void; cleanup: () => void }
/** One service instance is one authenticated session; disposing invalidates every operation. */
export class PeerService implements ApplicationTransport {
  private control: FramedChannel;
  private nextId = 0;
  private pending = new Map<string, Pending>();
  private streams = new Map<string, PeerEvents>();
  private sockets = new Set<PeerSocket>();
  private uploads = new Set<() => void>();
  private live = false;
  private stopped = false;
  constructor(private pc: RTCPeerConnection, channel: RTCDataChannel, sessionId: string, onready: () => void, private onlost: () => void) {
    this.control = new FramedChannel(channel);
    const timer = setTimeout(() => this.close(), 20_000);
    channel.addEventListener('open', () => { void this.control.send(JSON.stringify({ type: 'hello', protocolVersion: 1, sessionId })).catch(() => this.close()); });
    channel.addEventListener('close', () => this.close());
    this.control.onerror = () => this.close();
    this.control.onmessage = data => {
      try {
        if (typeof data !== 'string' || data.length > 16 * 1024 * 1024) throw new Error('Invalid control data');
        const msg = JSON.parse(data);
        if (!this.live) {
          if (msg.type !== 'ready' || msg.protocolVersion !== 1) throw new Error('Invalid peer handshake');
          clearTimeout(timer); this.live = true; onready(); return;
        }
        if (typeof msg.id !== 'string') throw new Error('Invalid control id');
        if (msg.type === 'response') {
          const item = this.pending.get(msg.id); if (!item) return;
          const reply = response(msg.status, msg.body); this.pending.delete(msg.id); item.cleanup(); item.resolve(reply);
        } else if (msg.type === 'subscribed') this.streams.get(msg.id)?.onopen?.(new Event('open'));
        else if (msg.type === 'event') {
          if (typeof msg.event !== 'string' || typeof msg.data !== 'string') throw new Error('Invalid peer event');
          this.streams.get(msg.id)?.event(msg.event, msg.data);
        } else if (msg.type === 'closed') {
          const stream = this.streams.get(msg.id); this.streams.delete(msg.id);
          if (stream) { stream.onerror?.(new Event('error')); this.close(); }
        } else throw new Error('Unknown control message');
      } catch { this.close(); }
    };
    this.cleanupHandshake = () => clearTimeout(timer);
  }
  private cleanupHandshake: () => void;
  private assertLive(): void { if (!this.live || this.stopped) throw new Error('Computer disconnected. Reconnecting…'); }
  private send(value: object): void { this.assertLive(); void this.control.send(JSON.stringify(value)).catch(() => this.close()); }
  async request(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    this.assertLive();
    const request = input instanceof Request ? new Request(input, init) : new Request(new URL(String(input), location.origin), init);
    const url = new URL(request.url);
    if (url.origin !== location.origin || !url.pathname.startsWith('/api/') || !['GET', 'POST'].includes(request.method)) throw new Error('Unsupported peer request');
    if (url.pathname === '/api/attachments') return this.upload(url, request);
    const bytes = new Uint8Array(await request.arrayBuffer());
    this.assertLive();
    if (bytes.length > 1024 * 1024) throw new Error('Request exceeds 1 MiB');
    if (request.signal.aborted) throw new DOMException('Aborted', 'AbortError');
    if (this.pending.size >= 32) throw new Error('Too many requests in progress');
    const id = String(++this.nextId);
    return new Promise((resolve, reject) => {
      const abort = () => { this.pending.delete(id); cleanup(); reject(new DOMException('Aborted', 'AbortError')); if (this.live) this.send({ type: 'cancel', id }); };
      const timer = setTimeout(() => { this.pending.delete(id); cleanup(); reject(new Error('Peer request timed out')); if (this.live) this.send({ type: 'cancel', id }); }, 30_000);
      const cleanup = () => { clearTimeout(timer); request.signal.removeEventListener('abort', abort); };
      request.signal.addEventListener('abort', abort, { once: true });
      this.pending.set(id, { resolve, reject, cleanup });
      this.send({ type: 'request', id, method: request.method, path: url.pathname + url.search, ...(bytes.length ? { body: base64(bytes), contentType: request.headers.get('Content-Type') || 'application/json' } : {}) });
    });
  }
  events(path: string): EventSourceLike {
    this.assertLive();
    if (this.streams.size >= 32) throw new Error('Too many active streams');
    const id = String(++this.nextId);
    const stream = new PeerEvents(() => { if (this.streams.delete(id) && this.live) this.send({ type: 'cancel', id }); });
    this.streams.set(id, stream); this.send({ type: 'subscribe', id, path }); return stream;
  }
  socket(input: string | URL): SocketLike {
    this.assertLive();
    if (this.sockets.size >= 32) throw new Error('Too many active terminals');
    const url = new URL(input, location.href);
    if (url.pathname !== '/api/terminal') throw new Error('Unsupported peer channel');
    const q = url.searchParams;
    const socket = new PeerSocket(this.pc.createDataChannel(`herdr-terminal-v1:${++this.nextId}`, { ordered: true }), {
      terminal: q.get('terminal'), cols: Number(q.get('cols')), rows: Number(q.get('rows')), observe: q.get('observe') === 'true', resumed: q.get('resumed') === 'true', theme: q.get('theme'),
    }, () => this.sockets.delete(socket));
    this.sockets.add(socket); return socket;
  }
  private async upload(url: URL, request: Request): Promise<Response> {
    this.assertLive();
    if (this.uploads.size >= 2) throw new Error('Two uploads are already in progress');
    const declared = Number(request.headers.get('Content-Length'));
    if (declared > 8 * 1024 * 1024) throw new Error('Upload exceeds 8 MiB');
    const channel = this.pc.createDataChannel(`herdr-upload-v1:${++this.nextId}`, { ordered: true });
    const wire = new FramedChannel(channel);
    return new Promise((resolve, reject) => {
      let done = false;
      const cleanup = () => { clearTimeout(timer); this.uploads.delete(cancel); request.signal.removeEventListener('abort', cancel); wire.close(); channel.close(); };
      const fail = (error: Error) => { if (done) return; done = true; cleanup(); reject(error); };
      const cancel = () => fail(new DOMException('Upload disconnected or aborted', 'AbortError'));
      const timer = setTimeout(() => fail(new Error('Upload timed out')), 60_000);
      this.uploads.add(cancel); request.signal.addEventListener('abort', cancel, { once: true });
      if (request.signal.aborted) { cancel(); return; }
      wire.onerror = cancel; channel.addEventListener('close', cancel);
      wire.onmessage = data => {
        try {
          if (typeof data !== 'string') throw new Error('Invalid upload response');
          const msg = JSON.parse(data); if (msg.type !== 'upload_result') throw new Error('Invalid upload response');
          const result = response(msg.status, msg.body); done = true; cleanup(); resolve(result);
        } catch (error) { fail(error instanceof Error ? error : new Error('Upload failed')); }
      };
      channel.addEventListener('open', () => { void (async () => {
        const bytes = new Uint8Array(await request.arrayBuffer()); this.assertLive();
        if (done) return;
        if (!bytes.length || bytes.length > 8 * 1024 * 1024) throw new Error('Upload must be between 1 byte and 8 MiB');
        await wire.send(JSON.stringify({ type: 'upload', terminal: url.searchParams.get('terminal'), size: bytes.length, contentType: request.headers.get('Content-Type') || 'application/octet-stream' }));
        for (let offset = 0; offset < bytes.length; offset += 16 * 1024) { if (done) return; await wire.send(bytes.subarray(offset, offset + 16 * 1024)); }
      })().catch(error => fail(error instanceof Error ? error : new Error('Upload failed'))); });
    });
  }
  close(): void {
    if (this.stopped) return;
    this.stopped = true; this.live = false; this.cleanupHandshake();
    for (const item of this.pending.values()) { item.cleanup(); item.reject(new Error('Computer disconnected')); } this.pending.clear();
    for (const stream of this.streams.values()) stream.onerror?.(new Event('error')); this.streams.clear();
    for (const socket of [...this.sockets]) socket.close();
    for (const cancel of [...this.uploads]) cancel();
    this.control.close(); this.pc.close(); this.onlost();
  }
}
