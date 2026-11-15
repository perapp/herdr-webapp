import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FramedChannel, decodeFrame, encodeFrame, FRAME_HEADER_BYTES, MAX_FRAME_BYTES } from './peerFrames';
import { PeerService } from './peerService';

// Both endpoints use the real framing codec, not unframed JSON shortcuts.
class Channel extends EventTarget {
  ordered = true;
  maxRetransmits = null;
  maxPacketLifeTime = null;
  readyState = 'connecting';
  binaryType = 'blob';
  bufferedAmount = 0;
  bufferedAmountLowThreshold = 0;
  frames: ArrayBuffer[] = [];
  peer?: Channel;
  send(frame: ArrayBuffer): void {
    if (this.readyState !== 'open') throw new Error('Channel not open');
    this.frames.push(frame.slice(0));
    this.peer?.dispatchEvent(new MessageEvent('message', { data: frame.slice(0) }));
  }
  open(): void { this.readyState = 'open'; this.dispatchEvent(new Event('open')); }
  close(): void {
    if (this.readyState === 'closed') return;
    this.readyState = 'closed'; this.dispatchEvent(new Event('close'));
  }
}
function pair() {
  const local = new Channel(), remote = new Channel();
  local.peer = remote; remote.peer = local; remote.readyState = 'open';
  const wire = new FramedChannel(remote as unknown as RTCDataChannel);
  const messages: (string | ArrayBuffer)[] = [];
  wire.onmessage = message => messages.push(message);
  return { local, remote, wire, messages };
}
function jsonMessages(endpoint: ReturnType<typeof pair>): Record<string, unknown>[] {
  return endpoint.messages.filter((m): m is string => typeof m === 'string').map(m => JSON.parse(m));
}
const services: PeerService[] = [];
const wires: FramedChannel[] = [];
function setup() {
  const control = pair(); wires.push(control.wire);
  const channels: (ReturnType<typeof pair> & { label: string; options?: RTCDataChannelInit })[] = [];
  const pc = {
    createDataChannel: vi.fn((label: string, options?: RTCDataChannelInit) => {
      const endpoint = { ...pair(), label, options }; channels.push(endpoint); wires.push(endpoint.wire);
      return endpoint.local as unknown as RTCDataChannel;
    }),
    close: vi.fn(),
  };
  const ready = vi.fn(), lost = vi.fn();
  const service = new PeerService(pc as unknown as RTCPeerConnection, control.local as unknown as RTCDataChannel, 'session-test', ready, lost);
  services.push(service);
  const activate = async () => { control.local.open(); await control.wire.send(JSON.stringify({ type: 'ready', protocolVersion: 1 })); };
  return { control, channels, pc, ready, lost, service, activate };
}
async function reply(wire: FramedChannel, value: object) { await wire.send(JSON.stringify(value)); }
function body(value: string) { return btoa(value); }

beforeEach(() => {
  vi.stubGlobal('location', new URL('https://app.example.test/'));
  if (typeof CloseEvent === 'undefined') vi.stubGlobal('CloseEvent', class extends Event {
    code: number; reason: string;
    constructor(type: string, init: CloseEventInit = {}) { super(type); this.code = init.code ?? 0; this.reason = init.reason ?? ''; }
  });
});
afterEach(() => {
  for (const service of services.splice(0)) service.close();
  for (const wire of wires.splice(0)) wire.close();
  vi.useRealTimers(); vi.unstubAllGlobals();
});

describe('PeerService session and control requests', () => {
  it('sends a framed hello on open and gates every operation until ready', async () => {
    const { service, control, ready } = setup();
    await expect(service.request('/api/session')).rejects.toThrow('disconnected');
    expect(() => service.events('/api/events')).toThrow('disconnected');
    expect(() => service.socket('/api/terminal')).toThrow('disconnected');
    expect(control.local.frames).toHaveLength(0);
    control.local.open();
    expect(jsonMessages(control)).toEqual([{ type: 'hello', protocolVersion: 1, sessionId: 'session-test' }]);
    expect(decodeFrame(control.local.frames[0]).kind).toBe(0);
    expect(ready).not.toHaveBeenCalled();
    await expect(service.request('/api/session')).rejects.toThrow('disconnected');
    await control.wire.send(JSON.stringify({ type: 'ready', protocolVersion: 1 }));
    expect(ready).toHaveBeenCalledOnce();
    const pending = service.request('/api/session');
    await vi.waitFor(() => expect(jsonMessages(control)).toHaveLength(2));
    await reply(control.wire, { type: 'response', id: '1', status: 200, body: body('{"ok":true}') });
    expect(await (await pending).json()).toEqual({ ok: true });
  });

  it.each([0, 2])('rejects handshake version %s without becoming ready', async version => {
    const { service, control, ready, lost } = setup(); control.local.open();
    await reply(control.wire, { type: 'ready', protocolVersion: version });
    expect(ready).not.toHaveBeenCalled(); expect(lost).toHaveBeenCalledOnce();
    await expect(service.request('/api/session')).rejects.toThrow('disconnected');
  });

  it('expires an unanswered hello', async () => {
    vi.useFakeTimers(); const { control, ready, lost, pc } = setup(); control.local.open();
    await vi.advanceTimersByTimeAsync(20_000);
    expect(ready).not.toHaveBeenCalled(); expect(lost).toHaveBeenCalledOnce(); expect(pc.close).toHaveBeenCalledOnce();
  });

  it('frames method, query, UTF-8 base64 body and content type, and decodes error responses', async () => {
    const { service, control, activate } = setup(); await activate();
    const payload = JSON.stringify({ text: 'hello 🌍' });
    const pending = service.request('/api/commands?terminal=t%2F1', { method: 'POST', headers: { 'Content-Type': 'application/custom+json' }, body: payload });
    await vi.waitFor(() => expect(jsonMessages(control)).toHaveLength(2));
    expect(jsonMessages(control)[1]).toEqual({ type: 'request', id: '1', method: 'POST', path: '/api/commands?terminal=t%2F1', contentType: 'application/custom+json', body: btoa(String.fromCharCode(...new TextEncoder().encode(payload))) });
    await reply(control.wire, { type: 'response', id: '1', status: 409, body: body('{"error":"conflict"}') });
    const response = await pending;
    expect(response.status).toBe(409); expect(await response.json()).toEqual({ error: 'conflict' });
  });

  it('rejects unsupported targets/methods and oversized bodies without sending', async () => {
    const { service, control, activate } = setup(); await activate();
    for (const [url, init] of [
      ['https://other.test/api/session', undefined], ['/other', undefined], ['/api/session', { method: 'DELETE' }],
      ['/api/commands', { method: 'POST', body: 'x'.repeat(1024 * 1024 + 1) }],
    ] as const) await expect(service.request(url, init)).rejects.toThrow();
    expect(jsonMessages(control)).toHaveLength(1);
  });

  it('cancels aborted requests, ignores their late responses and does not replay them', async () => {
    const { service, control, activate, lost } = setup(); await activate();
    const abort = new AbortController();
    const pending = service.request('/api/session', { signal: abort.signal });
    const rejected = expect(pending).rejects.toMatchObject({ name: 'AbortError' });
    await vi.waitFor(() => expect(jsonMessages(control)).toHaveLength(2));
    abort.abort(); await rejected;
    expect(jsonMessages(control)[2]).toEqual({ type: 'cancel', id: '1' });
    await reply(control.wire, { type: 'response', id: '1', status: 200, body: body('stale') });
    const next = service.request('/api/session');
    await vi.waitFor(() => expect(jsonMessages(control)).toHaveLength(4));
    expect(jsonMessages(control)[3]).toMatchObject({ type: 'request', id: '2' });
    await reply(control.wire, { type: 'response', id: '1', status: 200, body: body('stale again') });
    await reply(control.wire, { type: 'response', id: '2', status: 200, body: body('fresh') });
    expect(await (await next).text()).toBe('fresh'); expect(lost).not.toHaveBeenCalled();
    expect(jsonMessages(control).filter(m => m.type === 'request')).toHaveLength(2);
  });

  it('times out pending requests with a cancel frame', async () => {
    vi.useFakeTimers(); const { service, control, activate } = setup(); await activate();
    const pending = service.request('/api/session');
    const rejected = expect(pending).rejects.toThrow('Peer request timed out');
    await vi.waitFor(() => expect(jsonMessages(control)).toHaveLength(2));
    await vi.advanceTimersByTimeAsync(30_000); await rejected;
    expect(jsonMessages(control)[2]).toEqual({ type: 'cancel', id: '1' });
  });

  it('disconnect rejects all pending work, stops streams and never sends or replays offline', async () => {
    const { service, control, activate, lost, pc } = setup(); await activate();
    const first = service.request('/api/one'), second = service.request('/api/two');
    const rejected = Promise.all([expect(first).rejects.toThrow('Computer disconnected'), expect(second).rejects.toThrow('Computer disconnected')]);
    const stream = service.events('/api/events'); const error = vi.fn(); stream.onerror = error;
    await vi.waitFor(() => expect(jsonMessages(control).filter(m => m.type === 'request')).toHaveLength(2));
    control.local.close(); await rejected;
    const count = control.local.frames.length;
    await reply(control.wire, { type: 'response', id: '1', status: 200, body: body('stale') });
    await expect(service.request('/api/three')).rejects.toThrow('disconnected');
    expect(() => service.socket('/api/terminal')).toThrow('disconnected');
    stream.close(); service.close();
    expect(control.local.frames).toHaveLength(count); expect(error).toHaveBeenCalledOnce();
    expect(lost).toHaveBeenCalledOnce(); expect(pc.close).toHaveBeenCalledOnce();
    const fresh = setup(); await fresh.activate();
    expect(jsonMessages(fresh.control)).toEqual([{ type: 'hello', protocolVersion: 1, sessionId: 'session-test' }]);
  });
});

describe('PeerService terminal channels', () => {
  it('switches terminal channels without closing control, events or the peer', async () => {
    const { service, channels, control, pc, lost, activate } = setup();
    await activate();
    const events = service.events('/api/events');
    const eventLost = vi.fn(); events.onerror = eventLost;
    for (let pane = 0; pane < 6; pane++) {
      const socket = service.socket(`/api/terminal?terminal=t${pane}&cols=80&rows=24`);
      const endpoint = channels.at(-1)!;
      endpoint.local.open();
      await vi.waitFor(() => expect(socket.readyState).toBe(1));
      socket.close();
      expect(socket.readyState).toBe(3);
      expect(control.local.readyState).toBe('open');
      expect(lost).not.toHaveBeenCalled();
      expect(eventLost).not.toHaveBeenCalled();
      expect(pc.close).not.toHaveBeenCalled();
    }
    const pending = service.request('/api/state');
    await vi.waitFor(() => expect(jsonMessages(control).at(-1)?.type).toBe('request'));
    const id = jsonMessages(control).at(-1)!.id;
    await reply(control.wire, { type: 'response', id, status: 200, body: body('{"connected":true}') });
    expect(await (await pending).json()).toEqual({ connected: true });
    events.close();
  });

  it('attaches before opening, reconstructs 256 KiB binary frames in both directions and forbids offline sends', async () => {
    const { service, channels, activate } = setup(); await activate();
    const socket = service.socket('/api/terminal?terminal=t1&cols=80&rows=24&observe=true&resumed=true&theme=dark');
    const endpoint = channels[0], opened = vi.fn(), received = vi.fn(), closed = vi.fn();
    socket.onopen = opened; socket.onmessage = received; socket.onclose = closed;
    expect(endpoint.label).toBe('herdr-terminal-v1:1'); expect(endpoint.options).toEqual({ ordered: true });
    expect(() => socket.send('offline')).toThrow('disconnected'); expect(endpoint.local.frames).toHaveLength(0);
    endpoint.local.open(); await vi.waitFor(() => expect(opened).toHaveBeenCalledOnce());
    expect(jsonMessages(endpoint)).toEqual([{ terminal: 't1', cols: 80, rows: 24, observe: true, resumed: true, theme: 'dark' }]);
    const bytes = Uint8Array.from({ length: 256 * 1024 }, (_, i) => i % 251);
    socket.send(bytes);
    expect(endpoint.messages[1]).toBeInstanceOf(ArrayBuffer);
    expect(new Uint8Array(endpoint.messages[1] as ArrayBuffer)).toEqual(bytes);
    const frames = endpoint.local.frames.slice(1);
    expect(frames.length).toBeGreaterThan(1); expect(frames.every(f => f.byteLength <= MAX_FRAME_BYTES)).toBe(true);
    expect(frames.reduce((total, f) => total + decodeFrame(f).payload.byteLength, 0)).toBe(bytes.length);
    // Feed separately encoded fragments so the receiver must reconstruct, not forward chunks.
    const chunkSize = MAX_FRAME_BYTES - FRAME_HEADER_BYTES;
    for (let offset = 0; offset < bytes.length; offset += chunkSize) {
      endpoint.local.dispatchEvent(new MessageEvent('message', { data: encodeFrame({ id: 1, total: bytes.length, offset, kind: 1, payload: bytes.subarray(offset, offset + chunkSize) }) }));
      expect(received).toHaveBeenCalledTimes(offset + chunkSize >= bytes.length ? 1 : 0);
    }
    expect(new Uint8Array(received.mock.calls[0][0].data)).toEqual(bytes);
    service.close(); const count = endpoint.local.frames.length;
    expect(socket.readyState).toBe(3); expect(closed).toHaveBeenCalledOnce();
    expect(() => socket.send(bytes)).toThrow('disconnected'); expect(endpoint.local.frames).toHaveLength(count);
  });
});

describe('PeerService uploads', () => {
  it('limits simultaneous uploads to two and aborts both on disconnect', async () => {
    const { service, channels, activate } = setup(); await activate();
    const a = service.request('/api/attachments?terminal=t1', { method: 'POST', body: 'a' });
    const b = service.request('/api/attachments?terminal=t1', { method: 'POST', body: 'b' });
    const rejected = Promise.all([expect(a).rejects.toMatchObject({ name: 'AbortError' }), expect(b).rejects.toMatchObject({ name: 'AbortError' })]);
    await expect(service.request('/api/attachments', { method: 'POST', body: 'c' })).rejects.toThrow('Two uploads');
    expect(channels).toHaveLength(2); service.close(); await rejected;
    expect(channels.every(c => c.local.readyState === 'closed')).toBe(true);
  });

  it('checks declared and actual upload sizes before sending content', async () => {
    const { service, channels, activate } = setup(); await activate();
    await expect(service.request('/api/attachments', { method: 'POST', headers: { 'Content-Length': String(8 * 1024 * 1024 + 1) }, body: 'x' })).rejects.toThrow('8 MiB');
    expect(channels).toHaveLength(0);
    for (const payload of ['', 'x'.repeat(8 * 1024 * 1024 + 1)]) {
      const pending = service.request('/api/attachments', { method: 'POST', body: payload });
      const rejected = expect(pending).rejects.toThrow('between 1 byte and 8 MiB');
      const channel = channels[channels.length - 1]; channel.local.open(); await rejected;
      expect(channel.local.frames).toHaveLength(0); expect(channel.local.readyState).toBe('closed');
    }
  });

  it('sends upload metadata and binary chunks, then decodes its result', async () => {
    const { service, channels, activate } = setup(); await activate();
    const bytes = new Uint8Array(40_000).fill(123);
    const pending = service.request('/api/attachments?terminal=t1', { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: bytes });
    const endpoint = channels[0]; endpoint.local.open();
    await vi.waitFor(() => expect(endpoint.messages).toHaveLength(4));
    expect(endpoint.label).toBe('herdr-upload-v1:1'); expect(endpoint.options).toEqual({ ordered: true });
    expect(jsonMessages(endpoint)).toEqual([{ type: 'upload', terminal: 't1', size: bytes.length, contentType: 'application/octet-stream' }]);
    const chunks = endpoint.messages.slice(1) as ArrayBuffer[];
    expect(chunks.map(c => c.byteLength)).toEqual([16_384, 16_384, 7_232]);
    expect(new Uint8Array(await new Blob(chunks).arrayBuffer())).toEqual(bytes);
    await reply(endpoint.wire, { type: 'upload_result', status: 201, body: body('{"name":"uploaded"}') });
    const result = await pending; expect(result.status).toBe(201); expect(await result.json()).toEqual({ name: 'uploaded' });
    expect(endpoint.local.readyState).toBe('closed');
  });
});
