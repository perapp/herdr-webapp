import { afterEach, describe, expect, it, vi } from 'vitest';
import { channelBase, channelContext, channelInput, channelPath, storageKey } from './channel';
import { setApplicationTransport, transportEvents, transportRequest, transportSocket } from './transport';
import { loadView, saveView } from './view';

function at(path: string) { vi.stubGlobal('location', new URL(`https://relay.test${path}`)); }
afterEach(() => { setApplicationTransport(undefined); vi.unstubAllGlobals(); });
describe('channel isolation', () => {
  it('retains stable URL and storage identities exactly', () => {
    at('/');
    expect(channelBase()).toBe('/'); expect(channelPath('/api/session')).toBe('/api/session');
    expect(channelContext()).toBe('https://relay.test');
    expect(storageKey('herdr.appearance')).toBe('herdr.appearance');
    const input = new Request('https://relay.test/api/session'); expect(channelInput(input)).toBe(input);
    expect(channelBase('https://relay.test/mainland/')).toBe('/');
  });
  it('prefixes all native I/O without changing the exact Origin or duplicating paths', async () => {
    at('/main/');
    const fetch = vi.fn().mockResolvedValue(new Response('{}'));
    const EventSource = vi.fn(function () {}), WebSocket = vi.fn(function (url: string | URL) { return { url }; });
    vi.stubGlobal('fetch', fetch); vi.stubGlobal('EventSource', EventSource); vi.stubGlobal('WebSocket', WebSocket);
    await transportRequest('/api/logout'); transportEvents('/api/updates/events');
    transportSocket(new URL('wss://relay.test/api/terminal'));
    expect(fetch).toHaveBeenCalledWith('/main/api/logout', undefined);
    expect(EventSource).toHaveBeenCalledWith('/main/api/updates/events');
    expect(String(WebSocket.mock.calls[0]?.[0])).toBe('wss://relay.test/main/api/terminal');
    expect(channelPath('/main/run')).toBe('/main/run');
    expect(channelPath('/run')).toBe('/main/run'); expect(channelContext()).toBe('https://relay.test/main');
    expect(location.origin).toBe('https://relay.test');
    expect(channelInput('https://other.test/api/session')).toBe('https://other.test/api/session');
  });
  it('keeps peer service application routes channel-neutral', async () => {
    at('/main/');
    const adapter = { request: vi.fn().mockResolvedValue(new Response('{}')), events: vi.fn(), socket: vi.fn() };
    setApplicationTransport(adapter);
    await transportRequest('/api/session'); transportEvents('/api/events'); transportSocket('wss://relay.test/api/terminal');
    expect(adapter.request).toHaveBeenCalledWith('/api/session', undefined);
    expect(adapter.events).toHaveBeenCalledWith('/api/events');
    expect(adapter.socket).toHaveBeenCalledWith('wss://relay.test/api/terminal');
  });
  it('isolates persisted view state and restores stable keys when returning', () => {
    const values = new Map<string, string>();
    vi.stubGlobal('localStorage', { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value) });
    at('/'); const stable = loadView(); stable.space = 'stable'; saveView(stable);
    at('/main/'); expect(loadView().space).toBe('');
    const main = loadView(); main.space = 'main'; saveView(main);
    expect(values.has('herdr.main:herdr.browser-view.v1')).toBe(true);
    expect(storageKey('herdr-relay-identities-v1')).toBe('herdr.main:herdr-relay-identities-v1');
    at('/'); expect(loadView().space).toBe('stable');
  });
});
