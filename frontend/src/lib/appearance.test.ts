import { afterEach, describe, expect, it, vi } from 'vitest';
import { parseAppearance, resolveAppearance } from './appearance';

async function fixture(saved?: string, systemDark = false) {
  vi.resetModules();
  const values = new Map(saved ? [['herdr.appearance', saved]] : []);
  const storage = { getItem: vi.fn((key: string) => values.get(key) ?? null), setItem: vi.fn((key: string, value: string) => values.set(key, value)) };
  const media = Object.assign(new EventTarget(), { matches: systemDark });
  const window = new EventTarget();
  const root = { dataset: {} as Record<string, string> };
  vi.stubGlobal('localStorage', storage);
  vi.stubGlobal('matchMedia', vi.fn(() => media));
  vi.stubGlobal('window', window);
  vi.stubGlobal('document', { documentElement: root });
  const appearance = await import('./appearance');
  return { ...appearance, values, storage, media, window, root,
    system: (dark: boolean) => { media.matches = dark; media.dispatchEvent(new Event('change')); },
    external: (value: string | null, key: string | null = 'herdr.appearance') => {
      if (value === null) values.delete('herdr.appearance'); else values.set('herdr.appearance', value);
      window.dispatchEvent(Object.assign(new Event('storage'), { key }));
    },
  };
}
afterEach(() => vi.unstubAllGlobals());

describe('browser appearance preference', () => {
  it('accepts only System/Light/Dark and resolves explicit choices over the OS', () => {
    for (const value of [null, undefined, '', 'LIGHT', '{}', '<script>']) expect(parseAppearance(value)).toBe('system');
    expect(parseAppearance('light')).toBe('light'); expect(parseAppearance('dark')).toBe('dark');
    expect(resolveAppearance('system', true)).toBe(true);
    expect(resolveAppearance('system', false)).toBe(false);
    expect(resolveAppearance('light', true)).toBe(false);
    expect(resolveAppearance('dark', false)).toBe(true);
  });
  it('applies the saved preference before mounting and retains it across reloads', async () => {
    const f = await fixture('dark');
    expect(f.initializeAppearance()).toEqual({ preference: 'dark', dark: true });
    expect(f.root.dataset.theme).toBe('dark');
    expect(f.setAppearance('light')).toBe(true);
    expect(f.storage.setItem).toHaveBeenCalledWith('herdr.appearance', 'light');
    vi.resetModules();
    expect((await import('./appearance')).initializeAppearance()).toEqual({ preference: 'light', dark: false });
  });
  it('shares updates with all panes; follows OS changes only in System mode', async () => {
    const f = await fixture(); const app = vi.fn(), pane = vi.fn();
    const stopApp = f.watchAppearance(app), stopPane = f.watchAppearance(pane);
    f.system(true);
    expect(app).toHaveBeenLastCalledWith({ preference: 'system', dark: true });
    expect(pane).toHaveBeenLastCalledWith({ preference: 'system', dark: true });
    f.setAppearance('light'); f.system(true);
    expect(f.root.dataset.theme).toBe('light');
    expect(pane).toHaveBeenLastCalledWith({ preference: 'light', dark: false });
    f.setAppearance('system');
    expect(f.root.dataset.theme).toBe('dark');
    stopApp(); stopPane();
    const count = pane.mock.calls.length;
    f.system(false);
    expect(pane).toHaveBeenCalledTimes(count);
  });
  it('syncs another tab and browser-storage clearing without persisting credentials', async () => {
    const f = await fixture('light'); const update = vi.fn(); const stop = f.watchAppearance(update);
    f.external('dark'); expect(f.root.dataset.theme).toBe('dark');
    f.external('light', 'unrelated'); expect(f.root.dataset.theme).toBe('dark');
    f.external(null, null); expect(update).toHaveBeenLastCalledWith({ preference: 'system', dark: false });
    expect(f.storage.setItem).not.toHaveBeenCalled(); stop();
  });
  it('keeps appearance usable if storage is blocked and reports unsaved choices', async () => {
    const f = await fixture();
    f.storage.getItem.mockImplementation(() => { throw new Error('blocked'); });
    expect(f.initializeAppearance().preference).toBe('system');
    f.storage.setItem.mockImplementation(() => { throw new Error('blocked'); });
    expect(f.setAppearance('dark')).toBe(false);
    expect(f.root.dataset.theme).toBe('dark');
    expect(f.initializeAppearance()).toEqual({ preference: 'dark', dark: true });
  });
});
