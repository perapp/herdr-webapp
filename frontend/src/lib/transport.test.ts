import { afterEach, describe, expect, it, vi } from 'vitest';
import { disconnectedTransport, setApplicationTransport, transportEvents, transportRequest, transportSocket } from './transport';

afterEach(() => { setApplicationTransport(undefined); vi.unstubAllGlobals(); });

function nativeSpies() {
  const response = Promise.resolve(new Response('native'));
  const fetch = vi.fn().mockReturnValue(response);
  const EventSource = vi.fn(function () {});
  const WebSocket = vi.fn(function () {});
  vi.stubGlobal('fetch', fetch); vi.stubGlobal('EventSource', EventSource); vi.stubGlobal('WebSocket', WebSocket);
  return { fetch, EventSource, WebSocket, response };
}

describe('application transport factories', () => {
  it('preserves native arguments, instances and fetch promise without an adapter', () => {
    const native = nativeSpies();
    const input = new Request('https://example.test/api/session');
    const init = { signal: new AbortController().signal };
    const url = new URL('wss://example.test/api/terminal');
    expect(transportRequest(input, init)).toBe(native.response);
    expect(native.fetch).toHaveBeenCalledExactlyOnceWith(input, init);
    expect(transportEvents('/api/events')).toBe(native.EventSource.mock.instances[0]);
    expect(native.EventSource).toHaveBeenCalledExactlyOnceWith('/api/events');
    expect(transportSocket(url)).toBe(native.WebSocket.mock.instances[0]);
    expect(native.WebSocket).toHaveBeenCalledExactlyOnceWith(url);
  });

  it('routes only through the configured adapter and restores native factories when cleared', () => {
    const native = nativeSpies();
    const events = { onopen: null, onerror: null, addEventListener: vi.fn(), close: vi.fn() };
    const socket = { readyState: 1, binaryType: 'arraybuffer' as const, onopen: null, onmessage: null, onerror: null, onclose: null, send: vi.fn(), close: vi.fn() };
    const response = Promise.resolve(new Response('peer'));
    const adapter = { request: vi.fn().mockReturnValue(response), events: vi.fn().mockReturnValue(events), socket: vi.fn().mockReturnValue(socket) };
    setApplicationTransport(adapter);
    const input = new URL('https://example.test/api/session');
    const init = { method: 'POST', body: '{}' };
    expect(transportRequest(input, init)).toBe(response);
    expect(transportEvents('/api/events')).toBe(events);
    expect(transportSocket('wss://example.test/api/terminal')).toBe(socket);
    expect(adapter.request).toHaveBeenCalledExactlyOnceWith(input, init);
    expect(adapter.events).toHaveBeenCalledExactlyOnceWith('/api/events');
    expect(adapter.socket).toHaveBeenCalledExactlyOnceWith('wss://example.test/api/terminal');
    expect(native.fetch).not.toHaveBeenCalled(); expect(native.EventSource).not.toHaveBeenCalled(); expect(native.WebSocket).not.toHaveBeenCalled();
    setApplicationTransport(undefined);
    transportRequest(input); transportEvents('/api/events'); transportSocket('ws://example.test/api/terminal');
    expect(native.fetch).toHaveBeenCalledOnce(); expect(native.EventSource).toHaveBeenCalledOnce(); expect(native.WebSocket).toHaveBeenCalledOnce();
  });

  it('never falls back to native networking with disconnectedTransport', async () => {
    const native = nativeSpies();
    setApplicationTransport(disconnectedTransport);
    await expect(transportRequest('/api/session')).rejects.toThrow('Computer disconnected');
    expect(() => transportEvents('/api/events')).toThrow('Computer disconnected');
    expect(() => transportSocket('ws://example.test/api/terminal')).toThrow('Computer disconnected');
    expect(native.fetch).not.toHaveBeenCalled(); expect(native.EventSource).not.toHaveBeenCalled(); expect(native.WebSocket).not.toHaveBeenCalled();
  });
});
