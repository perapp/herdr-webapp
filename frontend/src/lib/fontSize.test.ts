import { afterEach, describe, expect, it, vi } from 'vitest';
import { defaultFontSize, fontSizeKey, maxFontSize, minFontSize, parseFontSize } from './fontSize';
async function fixture(saved?: string, main = false) {
  vi.resetModules();
  if (main) vi.stubGlobal('location', new URL('https://example.test/main/'));
  const key = main ? `herdr.main:${fontSizeKey}` : fontSizeKey;
  const values = new Map(saved === undefined ? [] : [[key, saved]]);
  const storage = { getItem: vi.fn((key: string) => values.get(key) ?? null), setItem: vi.fn((key: string, value: string) => values.set(key, value)) };
  const window = new EventTarget();
  const properties = new Map<string, string>();
  const root = { style: { setProperty: vi.fn((name: string, value: string) => properties.set(name, value)) } };
  vi.stubGlobal('document', { documentElement: root });
  vi.stubGlobal('localStorage', storage); vi.stubGlobal('window', window);
  const module = await import('./fontSize');
  return { ...module, values, storage, window, properties, root };
}
afterEach(() => vi.unstubAllGlobals());
describe('app-wide font size preference', () => {
  it('accepts bounded integer pixels and defaults safely for malformed settings', () => {
    for (const size of [minFontSize, 13, 18, maxFontSize]) {
      expect(parseFontSize(size)).toBe(size); expect(parseFontSize(String(size))).toBe(size);
    }
    for (const size of [null, undefined, '', '13px', '13.5', '1e1', '<script>', {}, [], NaN, Infinity, 13.5, 0, minFontSize - 1, maxFontSize + 1]) expect(parseFontSize(size)).toBe(defaultFontSize);
  });
  it('loads before terminal creation, persists and restores across reloads', async () => {
    const f = await fixture('18'); expect(f.initializeFontSize()).toBe(18);
    expect(f.properties.get('--app-font-size')).toBe('18px');
    expect(f.setFontSize(20)).toBe(true); expect(f.storage.setItem).toHaveBeenCalledWith(fontSizeKey, '20');
    expect(f.properties.get('--app-font-size')).toBe('20px');
    vi.resetModules(); expect((await import('./fontSize')).initializeFontSize()).toBe(20);
    expect(f.setFontSize(100)).toBe(false); expect(f.values.get(fontSizeKey)).toBe('20');
    expect(f.properties.get('--app-font-size')).toBe('20px');
  });
  it('updates every pane and releases subscriptions on disposal', async () => {
    const f = await fixture(); const one = vi.fn(), two = vi.fn();
    const stopOne = f.watchFontSize(one), stopTwo = f.watchFontSize(two);
    f.setFontSize(17); expect(one).toHaveBeenLastCalledWith(17); expect(two).toHaveBeenLastCalledWith(17);
    stopOne(); const count = one.mock.calls.length; f.setFontSize(19);
    expect(one).toHaveBeenCalledTimes(count); expect(two).toHaveBeenLastCalledWith(19); stopTwo();
  });
  it('syncs another browser tab and clearing preferences without storing credentials', async () => {
    const f = await fixture('16'); const update = vi.fn(); const stop = f.watchFontSize(update);
    f.values.set(fontSizeKey, '22'); f.window.dispatchEvent(Object.assign(new Event('storage'), { key: fontSizeKey }));
    expect(update).toHaveBeenLastCalledWith(22);
    f.values.set(fontSizeKey, '24'); f.window.dispatchEvent(Object.assign(new Event('storage'), { key: 'unrelated' }));
    expect(update).toHaveBeenLastCalledWith(22);
    f.values.delete(fontSizeKey); f.window.dispatchEvent(Object.assign(new Event('storage'), { key: null }));
    expect(update).toHaveBeenLastCalledWith(defaultFontSize); expect(f.storage.setItem).not.toHaveBeenCalled(); stop();
  });
  it('preserves main/stable preference isolation while scaling chrome', async () => {
    const f = await fixture('18', true);
    f.values.set(fontSizeKey, '9');
    expect(f.initializeFontSize()).toBe(18);
    expect(f.properties.get('--app-font-size')).toBe('18px');
    f.setFontSize(22);
    expect(f.values.get(`herdr.main:${fontSizeKey}`)).toBe('22');
    expect(f.values.get(fontSizeKey)).toBe('9');
    f.values.set(fontSizeKey, '28');
    f.window.dispatchEvent(Object.assign(new Event('storage'), { key: fontSizeKey }));
    expect(f.properties.get('--app-font-size')).toBe('22px');
  });
  it('keeps chrome synchronized without pane subscriptions and restores defaults on storage clear', async () => {
    const f = await fixture('17');
    f.initializeFontSize(); f.initializeFontSize();
    const listener = vi.fn(); const stop = f.watchFontSize(listener); stop();
    const count = listener.mock.calls.length;
    f.values.set(fontSizeKey, '28'); f.window.dispatchEvent(Object.assign(new Event('storage'), { key: fontSizeKey }));
    expect(f.properties.get('--app-font-size')).toBe('28px'); expect(listener).toHaveBeenCalledTimes(count);
    f.values.set(fontSizeKey, '<script>'); f.window.dispatchEvent(Object.assign(new Event('storage'), { key: fontSizeKey }));
    expect(f.properties.get('--app-font-size')).toBe('13px');
    f.values.clear(); f.window.dispatchEvent(Object.assign(new Event('storage'), { key: null }));
    expect(f.properties.get('--app-font-size')).toBe('13px');
  });
  it('still applies changes when browser storage is blocked', async () => {
    const f = await fixture(); f.storage.getItem.mockImplementation(() => { throw new Error('blocked'); });
    expect(f.initializeFontSize()).toBe(defaultFontSize); const update = vi.fn(); const stop = f.watchFontSize(update);
    f.storage.setItem.mockImplementation(() => { throw new Error('blocked'); });
    expect(f.setFontSize(21)).toBe(false); expect(update).toHaveBeenLastCalledWith(21); expect(f.initializeFontSize()).toBe(21);
    expect(f.properties.get('--app-font-size')).toBe('21px'); stop();
  });
});
