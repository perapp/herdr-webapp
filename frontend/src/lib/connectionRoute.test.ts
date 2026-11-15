import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { connectionRoute, connectionPath, lastComputer, rememberComputer, forgetSelection, ConnectionGeneration } from './connectionRoute';
import { blankView, forgetView, loadView, saveView } from './view';

const at = (path: string) => vi.stubGlobal('location', new URL(`https://relay.test${path}`));
let values: Map<string, string>;
beforeEach(() => {
  at('/'); values = new Map();
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  });
});
afterEach(() => vi.unstubAllGlobals());

describe('connection destinations', () => {
  it('keeps channel roots as Connections, independent of remembered selection', () => {
    rememberComputer('laptop');
    expect(connectionRoute()).toEqual({ kind: 'connections' });
    at('/main/'); rememberComputer('desktop');
    expect(connectionRoute()).toEqual({ kind: 'connections' });
    expect(connectionPath('laptop')).toBe('/main/connections/laptop');
    expect(connectionRoute('https://relay.test/main/connections/laptop?space=one')).toEqual({ kind: 'computer', id: 'laptop' });
    expect(connectionRoute('https://relay.test/connections/desktop')).toEqual({ kind: 'computer', id: 'desktop' });
  });
  it('recognizes launch separately and never treats reserved or malformed routes as computers', () => {
    for (const prefix of ['', '/main']) {
      expect(connectionRoute(`https://relay.test${prefix}/launch`)).toEqual({ kind: 'launch' });
      for (const path of ['/api/session', '/relay/socket', '/client/manifest.json', '/assets/index.js', '/connections/a/b', '/connections/a%2Fb', '/connections/', '/launch/other']) {
        expect(connectionRoute(`https://relay.test${prefix}${path}`)).toEqual({ kind: 'unknown' });
      }
    }
    expect(() => connectionPath('../api')).toThrow();
  });
  it('remembers explicit selections indefinitely per channel, even before any successful connection', () => {
    rememberComputer('offline');
    vi.spyOn(Date, 'now').mockReturnValue(9_000_000_000_000);
    expect(lastComputer()).toBe('offline');
    at('/connections/explicit');
    expect(connectionRoute()).toEqual({ kind: 'computer', id: 'explicit' });
    at('/main/launch'); expect(lastComputer()).toBe(''); rememberComputer('main-computer');
    at('/launch'); expect(lastComputer()).toBe('offline');
    forgetSelection('different'); expect(lastComputer()).toBe('offline');
    forgetSelection('offline'); expect(lastComputer()).toBe('');
    at('/main/launch'); expect(lastComputer()).toBe('main-computer');
    vi.restoreAllMocks();
  });
  it('isolates late callbacks, including callbacks fired synchronously during transport stop', () => {
    const generation = new ConnectionGeneration();
    const oldEvent = generation.guard();
    expect(oldEvent()).toBe(true);
    generation.invalidate();
    expect(oldEvent()).toBe(false);
    const nextEvent = generation.guard();
    expect(nextEvent()).toBe(true);
    generation.invalidate();
    expect(oldEvent()).toBe(false); expect(nextEvent()).toBe(false);
  });
});

describe('computer-local view state', () => {
  it('isolates overlapping layout IDs across computers, channels, and local direct mode', () => {
    const save = (path: string, space: string) => { at(path); const view = loadView(); view.space = space; view.focused.same = space; saveView(view); };
    save('/', 'local'); save('/connections/a', 'a'); save('/connections/b', 'b'); save('/main/connections/a', 'main-a');
    for (const [path, expected] of [['/', 'local'], ['/connections/a', 'a'], ['/connections/b', 'b'], ['/main/connections/a', 'main-a']]) {
      at(path); expect(loadView().space).toBe(expected); expect(loadView().focused.same).toBe(expected);
    }
  });
  it('binds saves to the loaded computer despite navigation before teardown', () => {
    at('/connections/a'); const view = loadView(); view.space = 'a';
    at('/connections/b'); saveView(new Proxy(view, {}));
    expect(loadView()).toEqual(blankView());
    at('/connections/a'); expect(loadView().space).toBe('a');
    expect(JSON.stringify(view)).not.toContain('browser-view-storage');
  });
  it('forgets only that computer’s view and handles corrupt or unavailable storage', () => {
    at('/connections/a'); const a = loadView(); a.space = 'a'; saveView(a);
    at('/connections/b'); const b = loadView(); b.space = 'b'; saveView(b);
    forgetView('a'); expect(loadView().space).toBe('b');
    at('/connections/a'); expect(loadView()).toEqual(blankView());
    values.set('herdr.browser-view.v1:computer:a', '{invalid'); expect(loadView()).toEqual(blankView());
    vi.stubGlobal('localStorage', { getItem() { throw Error('blocked'); }, setItem() { throw Error('blocked'); }, removeItem() { throw Error('blocked'); } });
    expect(loadView()).toEqual(blankView()); expect(lastComputer()).toBe('');
    expect(() => { saveView(blankView()); forgetView('a'); rememberComputer('a'); forgetSelection('a'); }).not.toThrow();
  });
});
