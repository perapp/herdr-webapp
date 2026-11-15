import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
const backend = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock('./api', () => ({ request: backend.request }));
import { disableNotifications, enableNotifications, notificationPresenceReporter } from './notifications';
import { disconnectedTransport, setApplicationTransport } from './transport';

let values: Map<string, string>;
let subscription: { endpoint: string; options: object; toJSON: ReturnType<typeof vi.fn>; unsubscribe: ReturnType<typeof vi.fn> };
let registration: { scope: string; active: object; pushManager: { getSubscription: ReturnType<typeof vi.fn>; subscribe: ReturnType<typeof vi.fn> }; unregister: ReturnType<typeof vi.fn> };
let permission: ReturnType<typeof vi.fn>;
let register: ReturnType<typeof vi.fn>;
beforeEach(() => {
  vi.stubGlobal('location', new URL('https://herdr.test/main/connections/computer-a'));
  values = new Map();
  vi.stubGlobal('localStorage', { getItem: (k: string) => values.get(k) || null, setItem: (k: string, v: string) => values.set(k, v), removeItem: (k: string) => values.delete(k) });
  subscription = { endpoint: 'https://fcm.googleapis.com/example', options: {}, toJSON: vi.fn(() => ({ endpoint: 'private-endpoint' })), unsubscribe: vi.fn().mockResolvedValue(true) };
  registration = { scope: 'https://herdr.test/main/notifications/computer-a/', active: {}, pushManager: { getSubscription: vi.fn().mockResolvedValue(subscription), subscribe: vi.fn().mockResolvedValue(subscription) }, unregister: vi.fn().mockResolvedValue(true) };
  register = vi.fn().mockResolvedValue(registration);
  vi.stubGlobal('navigator', { serviceWorker: { register, getRegistrations: vi.fn().mockResolvedValue([registration]) }, userAgent: 'Chrome Android', platform: 'Linux', maxTouchPoints: 5 });
  vi.stubGlobal('window', { isSecureContext: true, PushManager: {}, Notification: {}, setTimeout });
  vi.stubGlobal('matchMedia', () => ({ matches: false }));
  permission = vi.fn().mockResolvedValue('granted');
  vi.stubGlobal('Notification', { permission: 'granted', requestPermission: permission });
  const bytes = new Uint8Array(65); bytes[0] = 4;
  const publicKey = btoa(String.fromCharCode(...bytes)).replace(/=+$/, '').replace(/\+/g, '-').replace(/\//g, '_');
  backend.request.mockReset().mockImplementation(async (_path, body) => body ? { ok: true } : { supported: true, publicKey });
});
afterEach(() => { vi.unstubAllGlobals(); setApplicationTransport(undefined); });
describe('per-computer notification lifecycle', () => {
  it('requests permission immediately from a gesture and uses a separate secret-free channel/computer registration', async () => {
    const action = enableNotifications();
    expect(permission).toHaveBeenCalledOnce();
    await action;
    expect(register).toHaveBeenCalledWith('/main/sw.js', { scope: '/main/notifications/computer-a/', updateViaCache: 'none' });
    expect(backend.request).toHaveBeenCalledWith('/api/notifications', { subscription: { endpoint: 'private-endpoint' }, connectionId: 'computer-a', channel: 'main' }, undefined, expect.any(Function));
    expect(values.get('herdr.main:herdr.notifications.v1:computer-a')).toBe('enabled');
  });
  it('removes the old authenticated endpoint when repairing a rotated VAPID key', async () => {
    subscription.options = { applicationServerKey: new Uint8Array(65).buffer };
    await enableNotifications();
    expect(backend.request).toHaveBeenCalledWith('/api/notifications/delete', { endpoint: subscription.endpoint }, undefined, expect.any(Function));
    expect(subscription.unsubscribe).toHaveBeenCalledOnce();
    expect(registration.pushManager.subscribe).toHaveBeenCalledOnce();
  });
  it('invalidates delivery if its app preference cannot be persisted', async () => {
    vi.stubGlobal('localStorage', { getItem: () => null, setItem: () => { throw new Error('full'); } });
    await expect(enableNotifications()).rejects.toThrow('storage');
    expect(subscription.unsubscribe).toHaveBeenCalledOnce();
    expect(backend.request).toHaveBeenCalledWith('/api/notifications/delete', { endpoint: subscription.endpoint }, undefined, expect.any(Function));
  });
  it('does not subscribe or save preference after denied permission', async () => {
    permission.mockResolvedValue('denied');
    await expect(enableNotifications()).rejects.toThrow('permission');
    expect(register).not.toHaveBeenCalled(); expect(values.size).toBe(0);
  });
  it('disables server registration before unsubscribing the browser', async () => {
    await enableNotifications(); backend.request.mockClear();
    await disableNotifications();
    expect(backend.request).toHaveBeenCalledWith('/api/notifications/delete', { endpoint: subscription.endpoint }, undefined, expect.any(Function));
    expect(subscription.unsubscribe).toHaveBeenCalledOnce();
    expect(registration.unregister).toHaveBeenCalledOnce(); expect(values.size).toBe(0);
  });
  it('does not claim disabled when authenticated removal fails', async () => {
    await enableNotifications();
    backend.request.mockRejectedValue(new Error('offline'));
    await expect(disableNotifications()).rejects.toThrow('offline');
    expect(subscription.unsubscribe).not.toHaveBeenCalled(); expect(values.size).toBe(1);
  });
  it('binds async permission and registration work to original computer and transport', async () => {
    const a = { ...disconnectedTransport, request: vi.fn() }, b = { ...disconnectedTransport, request: vi.fn() };
    setApplicationTransport(a);
    let grant!: (value: string) => void;
    permission.mockImplementation(() => new Promise<string>(resolve => { grant = resolve; }));
    const action = enableNotifications();
    setApplicationTransport(b);
    vi.stubGlobal('location', new URL('https://herdr.test/main/connections/computer-b'));
    grant('granted'); await action;
    expect(register).toHaveBeenCalledWith('/main/sw.js', expect.objectContaining({ scope: '/main/notifications/computer-a/' }));
    const sender = backend.request.mock.calls[0][3];
    sender('/api/test'); expect(a.request).toHaveBeenCalledOnce(); expect(b.request).not.toHaveBeenCalled();
    expect(values.has('herdr.main:herdr.notifications.v1:computer-b')).toBe(false);
  });
  it('presence reports only a requested terminal with client-scoped lease, never after opt-out', async () => {
    const report = notificationPresenceReporter();
    await report('t1'); expect(backend.request).not.toHaveBeenCalled();
    await enableNotifications(); backend.request.mockClear();
    await report('beta/t1');
    expect(backend.request).toHaveBeenCalledWith('/api/notifications/seen', { endpoint: subscription.endpoint, terminal: 'beta/t1', clientId: expect.any(String) }, undefined, expect.any(Function));
    await report('');
    expect(backend.request.mock.calls.at(-1)?.[1].terminal).toBe('');
  });
});
