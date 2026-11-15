import { afterEach, describe, expect, it, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { installExplanation } from './pwa';

afterEach(() => vi.unstubAllGlobals());
const env = { secure: true, standalone: false, userAgent: '', platform: '', touchPoints: 0 };
describe('install lifecycle', () => {
  async function setup(path = '/main/connections/secret-free-id') {
    vi.resetModules();
    const handlers: Record<string, (event: Event) => void> = {};
    const register = vi.fn().mockResolvedValue({});
    const append = vi.fn();
    vi.stubGlobal('location', new URL(`https://herdr.test${path}`));
    vi.stubGlobal('window', { isSecureContext: true, addEventListener: (name: string, cb: (event: Event) => void) => { handlers[name] = cb; } });
    vi.stubGlobal('document', { createElement: () => ({}), head: { append } });
    vi.stubGlobal('navigator', { serviceWorker: { register }, userAgent: 'Chrome', platform: 'Linux', maxTouchPoints: 0 });
    vi.stubGlobal('matchMedia', () => ({ matches: false }));
    const pwa = await import('./pwa');
    pwa.initializePwa();
    return { ...pwa, handlers, register, append };
  }
  it('registers only channel URLs without connection credentials and initializes once', async () => {
    const pwa = await setup('/main/connections/example?token=not-forwarded#private');
    pwa.initializePwa();
    expect(pwa.register).toHaveBeenCalledExactlyOnceWith('/main/sw.js', { scope: '/main/', updateViaCache: 'none' });
    expect(pwa.append).toHaveBeenCalledExactlyOnceWith({ rel: 'manifest', href: '/main/manifest.webmanifest' }, { rel: 'apple-touch-icon', href: '/main/icons/icon-192.png' });
  });
  it('invokes retained native prompt synchronously from the gesture and clears dismissed prompts', async () => {
    const pwa = await setup('/');
    const preventDefault = vi.fn();
    const prompt = vi.fn().mockResolvedValue(undefined);
    pwa.handlers.beforeinstallprompt({ preventDefault, prompt, userChoice: Promise.resolve({ outcome: 'dismissed' }) } as unknown as Event);
    const outcome = pwa.installApp();
    expect(prompt).toHaveBeenCalledOnce(); expect(preventDefault).toHaveBeenCalledOnce();
    expect(await outcome).toContain('dismissed');
    expect(await pwa.installApp()).toContain('not offered an install prompt');
    expect(prompt).toHaveBeenCalledOnce();
  });
  it('clears installed and failed prompt state without hiding the action', async () => {
    const pwa = await setup();
    const prompt = vi.fn().mockRejectedValue(new Error('blocked'));
    const installEvent = { preventDefault: vi.fn(), prompt, userChoice: Promise.resolve({ outcome: 'accepted' }) } as unknown as Event;
    pwa.handlers.beforeinstallprompt(installEvent);
    expect(await pwa.installApp()).toContain('could not start installation');
    pwa.handlers.beforeinstallprompt(installEvent);
    pwa.handlers.appinstalled(new Event('appinstalled'));
    expect(await pwa.installApp()).toContain('not offered an install prompt');
    expect(prompt).toHaveBeenCalledOnce();
  });
});
describe('install instructions', () => {
  it('explains standalone, insecure, embedded and unknown environments honestly', () => {
    expect(installExplanation({ ...env, standalone: true })).toContain('already opened as an app');
    expect(installExplanation({ ...env, secure: false })).toContain('HTTPS');
    expect(installExplanation({ ...env, userAgent: 'Android; wv) Chrome' })).toContain('embedded browser');
    expect(installExplanation(env)).toContain('reason is unknown');
  });
  it('recognizes desktop-style iPad and Android manual installation', () => {
    expect(installExplanation({ ...env, platform: 'MacIntel', touchPoints: 5 })).toContain('Share menu');
    expect(installExplanation({ ...env, userAgent: 'Android Chrome' })).toContain('Samsung Internet');
    expect(installExplanation({ ...env, userAgent: 'Firefox' })).toContain('Desktop Firefox');
  });
  it('ships the same channel-aware worker in both embedded and Vite main paths', () => {
    expect(readFileSync(new URL('../../public/main/sw.js', import.meta.url), 'utf8')).toBe(readFileSync(new URL('../../public/sw.js', import.meta.url), 'utf8'));
  });
  it('keeps distinct stable/main app identities and secret-free channel launch routes', () => {
    for (const base of ['', 'main/']) {
      const manifest = JSON.parse(readFileSync(new URL(`../../public/${base}manifest.webmanifest`, import.meta.url), 'utf8'));
      expect(manifest.id).toBe(`/${base}launch`);
      expect(manifest.start_url).toBe(`/${base}launch`);
      expect(manifest.scope).toBe(`/${base}`);
      expect(manifest.icons.some((icon: { purpose: string }) => icon.purpose === 'maskable')).toBe(true);
    }
  });
});

function worker(scope = '/', script = '/sw.js') {
  const handlers: Record<string, (event: unknown) => void> = {};
  const showNotification = vi.fn().mockResolvedValue(undefined);
  const openWindow = vi.fn().mockResolvedValue(undefined);
  const matchAll = vi.fn().mockResolvedValue([]);
  const source = readFileSync(new URL('../../public/sw.js', import.meta.url), 'utf8');
  runInNewContext(source, {
    URL,
    self: {
      location: { href: `https://herdr.test${script}` },
      registration: { scope: `https://herdr.test${scope}`, showNotification },
      clients: { openWindow, matchAll },
      addEventListener: (name: string, cb: (event: unknown) => void) => { handlers[name] = cb; },
    },
  });
  async function push(payload: unknown) {
    let result: Promise<unknown> | undefined;
    handlers.push({ data: { json: () => payload }, waitUntil: (promise: Promise<unknown>) => { result = promise; } });
    await result;
  }
  async function click(url: string) {
    let result: Promise<unknown> | undefined;
    handlers.notificationclick({ notification: { data: { url }, close: vi.fn() }, waitUntil: (promise: Promise<unknown>) => { result = promise; } });
    await result;
  }
  return { handlers, showNotification, openWindow, matchAll, push, click };
}
describe('push-only service worker', () => {
  it('does not intercept fetch or force activation/reload', () => {
    expect(Object.keys(worker().handlers).sort()).toEqual(['notificationclick', 'push']);
    const source = readFileSync(new URL('../../public/sw.js', import.meta.url), 'utf8');
    expect(source).not.toMatch(/self\.(skipWaiting|clients\.claim)\(/);
    expect(readFileSync(new URL('../../public/main/sw.js', import.meta.url), 'utf8')).toBe(source);
  });
  it('builds only secret-free scoped destinations and generic lock-screen text', async () => {
    const sw = worker('/main/notifications/computer-1/', '/main/sw.js');
    await sw.push({ version: 1, eventId: 'event-1', kind: 'blocked', channel: 'main', connectionId: 'computer-1', terminal: 'local/pane_2' });
    expect(sw.showNotification).toHaveBeenCalledWith('Agent needs input', expect.objectContaining({
      body: 'Open Herdr to view this agent.',
      data: { url: 'https://herdr.test/main/connections/computer-1?terminal=local%2Fpane_2' },
    }));
    const valid = { version: 1, eventId: 'e1', kind: 'done', channel: 'main', connectionId: 'computer-1', terminal: 'local/pane' };
    for (const payload of [null, { ...valid, connectionId: '../api' }, { ...valid, token: 'secret' }, { ...valid, kind: 'working' }, { ...valid, terminal: 'x#token=secret' }, { ...valid, channel: 'stable' }, { ...valid, connectionId: 'other-computer' }, { ...valid, version: 2 }, { ...valid, terminal: 'host/../api' }]) await sw.push(payload);
    expect(sw.showNotification).toHaveBeenCalledTimes(1);
  });
  it('accepts native dotted machine names without accepting traversal segments', async () => {
    const sw = worker('/notifications/a/', '/sw.js');
    await sw.push({ version: 1, eventId: 'event', kind: 'done', connectionId: 'a', channel: 'stable', terminal: 'build.example/t7' });
    expect(sw.showNotification).toHaveBeenCalledOnce();
    await sw.click('https://herdr.test/connections/a?terminal=build.example%2Ft7');
    expect(sw.openWindow).toHaveBeenCalledWith('https://herdr.test/connections/a?terminal=build.example%2Ft7');
    await sw.push({ version: 1, eventId: 'bad', kind: 'done', connectionId: 'a', channel: 'stable', terminal: '../t7' });
    expect(sw.showNotification).toHaveBeenCalledOnce();
  });
  it('rejects off-origin, cross-channel, credentials and malformed notification click URLs', async () => {
    const sw = worker('/main/', '/main/sw.js');
    for (const url of ['https://evil.test/main/connections/ok', 'https://herdr.test/connections/ok', 'https://herdr.test/main/connections/ok#token=x', 'https://herdr.test/main/connections/ok?token=x', 'https://herdr.test/main/connections/ok?pane=a&pane=b', 'https://herdr.test/main/api/session', 'https://user:pass@herdr.test/main/connections/ok', 'https://herdr.test/main/connections/%2e%2e']) await sw.click(url);
    expect(sw.openWindow).not.toHaveBeenCalled();
    expect(sw.matchAll).not.toHaveBeenCalled();
    const stable = worker('/');
    await stable.click('https://herdr.test/main/connections/ok?terminal=t');
    expect(stable.openWindow).not.toHaveBeenCalled();
    const scoped = worker('/main/notifications/ok/', '/main/sw.js');
    await scoped.click('https://herdr.test/main/connections/other?terminal=t');
    expect(scoped.openWindow).not.toHaveBeenCalled();
  });
  it('focuses an exact existing destination without navigating another live terminal', async () => {
    const sw = worker('/main/notifications/a/', '/main/sw.js');
    const url = 'https://herdr.test/main/connections/a?terminal=b';
    const focus = vi.fn().mockResolvedValue(undefined);
    sw.matchAll.mockResolvedValue([{ url, focus }]);
    await sw.click(url);
    expect(focus).toHaveBeenCalledOnce(); expect(sw.openWindow).not.toHaveBeenCalled();
    sw.matchAll.mockResolvedValue([{ url: 'https://herdr.test/main/connections/other', focus }]);
    await sw.click(url);
    expect(sw.openWindow).toHaveBeenCalledWith(url);
    expect(focus).toHaveBeenCalledOnce();
  });
});
