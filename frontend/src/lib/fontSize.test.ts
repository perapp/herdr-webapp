import { afterEach, describe, expect, it, vi } from 'vitest';
import { defaultFontSize, fontSizeKey, maxFontSize, minFontSize, parseFontSize } from './fontSize';
async function fixture(saved?: string) {
  vi.resetModules();
  const values = new Map(saved === undefined ? [] : [[fontSizeKey, saved]]);
  const storage = { getItem: vi.fn((key: string) => values.get(key) ?? null), setItem: vi.fn((key: string, value: string) => values.set(key, value)) };
  const window = new EventTarget();
  vi.stubGlobal('localStorage', storage); vi.stubGlobal('window', window);
  const module = await import('./fontSize');
  return { ...module, values, storage, window };
}
afterEach(() => vi.unstubAllGlobals());
describe('terminal font size preference', () => {
  it('accepts bounded integer pixels and defaults safely for malformed settings', () => {
    for (const size of [minFontSize, 13, 18, maxFontSize]) {
      expect(parseFontSize(size)).toBe(size); expect(parseFontSize(String(size))).toBe(size);
    }
    for (const size of [null, undefined, '', '13px', '13.5', '1e1', '<script>', {}, [], NaN, Infinity, 13.5, 0, minFontSize - 1, maxFontSize + 1]) expect(parseFontSize(size)).toBe(defaultFontSize);
  });
  it('loads before terminal creation, persists and restores across reloads', async () => {
    const f = await fixture('18'); expect(f.initializeFontSize()).toBe(18);
    expect(f.setFontSize(20)).toBe(true); expect(f.storage.setItem).toHaveBeenCalledWith(fontSizeKey, '20');
    vi.resetModules(); expect((await import('./fontSize')).initializeFontSize()).toBe(20);
    expect(f.setFontSize(100)).toBe(false); expect(f.values.get(fontSizeKey)).toBe('20');
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
  it('still applies changes when browser storage is blocked', async () => {
    const f = await fixture(); f.storage.getItem.mockImplementation(() => { throw new Error('blocked'); });
    expect(f.initializeFontSize()).toBe(defaultFontSize); const update = vi.fn(); const stop = f.watchFontSize(update);
    f.storage.setItem.mockImplementation(() => { throw new Error('blocked'); });
    expect(f.setFontSize(21)).toBe(false); expect(update).toHaveBeenLastCalledWith(21); expect(f.initializeFontSize()).toBe(21); stop();
  });
});
