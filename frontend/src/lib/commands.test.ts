import { afterEach, describe, expect, it, vi } from 'vitest';
import { AGENT_KINDS, COMMANDS, FAVORITES_KEY, FAVORITES_LIMIT, detectCommandAgent, isCommandText, loadFavorites, saveFavorites, validateFavorites, fetchLiveCommands, validateLiveCommands, mergeCommands, LIVE_COMMAND_LIMIT } from './commands';

function storageFixture(initial?: string) {
  const values = new Map<string, string>(initial === undefined ? [] : [[FAVORITES_KEY, initial]]);
  const storage = { getItem: vi.fn((key: string) => values.get(key) ?? null), setItem: vi.fn((key: string, value: string) => { values.set(key, value); }) };
  vi.stubGlobal('localStorage', storage);
  return storage;
}
afterEach(() => vi.unstubAllGlobals());

describe('live command agent detection', () => {
  it('uses only exact current status.app values on live terminals', () => {
    for (const agent of AGENT_KINDS) expect(detectCommandAgent({ state: 'live', status: { app: agent } })).toBe(agent);
    expect(detectCommandAgent({ state: 'live', status: { app: 'claude-code' } })).toBe('claude');
    for (const app of ['PI', 'pi-dev', 'kiro', 'claude-unknown', '', null, {}, '/bin/codex']) {
      expect(detectCommandAgent({ state: 'live', status: { app }, title: 'pi', name: 'codex', argv: ['claude'] })).toBeUndefined();
    }
  });
  it('rejects missing, dead, cleared and explicitly unreported status', () => {
    expect(detectCommandAgent(undefined)).toBeUndefined();
    for (const state of [undefined, 'exited', 'lost', 'dead']) expect(detectCommandAgent({ state, status: { app: 'pi' } })).toBeUndefined();
    for (const status of [undefined, null, [], 'pi', {}]) expect(detectCommandAgent({ state: 'live', status, title: 'pi' })).toBeUndefined();
    expect(detectCommandAgent({ state: 'live', reported: false, status: { app: 'pi' } })).toBeUndefined();
  });
});

describe('static catalog and terminal text', () => {
  it('contains only unique validated slash commands; Pi fallback names match verified builtins', () => {
    for (const agent of AGENT_KINDS) {
      expect(new Set(COMMANDS[agent].map(c => c.id)).size).toBe(COMMANDS[agent].length);
      for (const command of COMMANDS[agent]) {
        expect(command.id).toBe(command.text);
        expect(isCommandText(command.text)).toBe(true);
        expect(command.description.length).toBeGreaterThan(0);
      }
    }
    expect(COMMANDS.pi.map(c => c.text)).toEqual(['/compact', '/new', '/model', '/session', '/tree', '/share', '/copy', '/reload']);
  });
  it('allows single-line slash names and bounded arguments, never terminal controls', () => {
    for (const text of ['/model', '/a.b:c-d_e', '/model provider/model', '/compact explain "why"', '/x ' + 'a'.repeat(512)]) expect(isCommandText(text)).toBe(true);
    for (const text of [undefined, 5, {}, '', 'model', ' /model', '//model', '/x ' + 'a'.repeat(513), '/' + 'a'.repeat(129), '/x\n', '/x\r', '/x\targ', '/x\0', '/x\x1b[31m', '/x\x7f', '/x\x85', '/x\u2028', '/x\u2029', '/x\u202e']) expect(isCommandText(text)).toBe(false);
    for (let code = 0; code < 160; code++) if (code < 32 || code >= 127) expect(isCommandText(`/x ${String.fromCharCode(code)}`)).toBe(false);
  });
});

describe('live Pi catalog', () => {
  const row = { name: 'git-mode', description: 'Toggle git authorization', source: 'extension' };
  const response = { available: true, message: 'Current Pi commands', commands: [row] };
  it('validates publisher names, descriptions, sources and envelope bounds', () => {
    expect(validateLiveCommands(response)).toEqual({ available: true, message: response.message, commands: [
      { id: '/git-mode', text: '/git-mode', description: row.description, source: 'extension' },
    ] });
    for (const source of ['extension', 'template', 'skill']) {
      expect(validateLiveCommands({ ...response, commands: [{ ...row, source, name: 'skill:aws-operations' }] }).commands[0].source).toBe(source);
    }
    for (const bad of [null, [], {}, { ...response, available: 'true' }, { ...response, message: 1 },
      { ...response, message: 'x'.repeat(2049) }, { ...response, message: '\u001b[31m' },
      { ...response, commands: Array(LIVE_COMMAND_LIMIT + 1).fill(row) }, { ...response, commands: {} }]) {
      expect(() => validateLiveCommands(bad)).toThrow('invalid');
    }
    for (const name of ['/git-mode', '-bad', '', 'a'.repeat(129), 'x arg', 'x\n', 'x\u001b', 'x\u202e', 'x/../y']) {
      expect(() => validateLiveCommands({ ...response, commands: [{ ...row, name }] })).toThrow('invalid');
    }
    for (const description of [null, 'a'.repeat(201), 'line\nline', '\u0085', '\u202e', '\u001b[31m']) {
      expect(() => validateLiveCommands({ ...response, commands: [{ ...row, description }] })).toThrow('invalid');
    }
    for (const source of ['builtin', '__proto__', null]) {
      expect(() => validateLiveCommands({ ...response, commands: [{ ...row, source }] })).toThrow('invalid');
    }
  });
  it('never returns commands from an unavailable publisher', () => {
    expect(validateLiveCommands({ ...response, available: false, message: 'Publisher stale; /reload then retry.' })).toEqual({
      available: false, message: 'Publisher stale; /reload then retry.', commands: [],
    });
  });
  it('merges only Pi runtime commands, deduplicates IDs and keeps builtins on collisions', () => {
    const live = validateLiveCommands({ ...response, commands: [row, row, { ...row, name: 'reload' }] }).commands;
    const catalog = mergeCommands('pi', live);
    expect(catalog).toHaveLength(COMMANDS.pi.length + 1);
    expect(catalog.filter(command => command.id === '/git-mode')).toHaveLength(1);
    expect(catalog.find(command => command.id === '/reload')).toEqual(COMMANDS.pi.find(command => command.id === '/reload'));
    expect(mergeCommands('codex', live)).toEqual(COMMANDS.codex);
    expect(mergeCommands('pi')).toEqual(COMMANDS.pi);
    const favorites = validateFavorites({ pi: ['/absent', '/git-mode'] });
    expect(catalog.filter(command => favorites.pi.includes(command.id)).map(command => command.id)).toEqual(['/git-mode']);
    expect(mergeCommands('pi').filter(command => favorites.pi.includes(command.id))).toEqual([]);
  });
  it('uses authenticated no-store GET, encoded terminal identity and the supplied abort signal', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify(response)));
    vi.stubGlobal('fetch', fetch);
    const controller = new AbortController();
    await expect(fetchLiveCommands('space/tab/pane?x=&', controller.signal)).resolves.toMatchObject({ available: true });
    expect(fetch).toHaveBeenCalledWith('/api/commands?terminal=space%2Ftab%2Fpane%3Fx%3D%26', expect.objectContaining({
      method: 'GET', credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal: controller.signal,
    }));
  });
  it('propagates backend errors and refuses aborted results even if transport finishes', async () => {
    const controller = new AbortController();
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: 'Publisher unavailable; reload Pi.' }), { status: 503 })));
    await expect(fetchLiveCommands('pane', controller.signal)).rejects.toThrow('Publisher unavailable; reload Pi.');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(response))));
    const pending = fetchLiveCommands('pane', controller.signal);
    controller.abort();
    await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
  });
});

describe('per-agent browser favorites', () => {
  it('validates containers, deduplicates and retains safe temporarily unavailable IDs', () => {
    for (const value of [undefined, null, [], 'pi', 5]) expect(validateFavorites(value)).toEqual({ pi: [], claude: [], codex: [], opencode: [] });
    expect(validateFavorites({ pi: ['/new', '/new', '/obsolete', '/models', '\u001b', {}, 1], codex: ['/review'], claude: 'bad' })).toEqual({ pi: ['/new', '/obsolete', '/models'], claude: [], codex: ['/review'], opencode: [] });
    expect(validateFavorites({ pi: [...Array(FAVORITES_LIMIT).fill('/obsolete'), '/new'] }).pi).toEqual(['/obsolete']);
  });
  it('retains custom IDs across reloads without storing arguments or unsafe text', () => {
    storageFixture();
    saveFavorites('pi', ['/git-mode', '/skill:aws-operations', '/git-mode', '/model secret', '/x\n', '/x\u001b', '//bad', '/-bad']);
    expect(loadFavorites().favorites.pi).toEqual(['/git-mode', '/skill:aws-operations']);
    saveFavorites('codex', ['/manual']);
    expect(loadFavorites().favorites.pi).toEqual(['/git-mode', '/skill:aws-operations']);
    expect(loadFavorites().favorites.codex).toEqual(['/manual']);
  });
  it('persists across loads and preserves other agents when saving', () => {
    const storage = storageFixture();
    expect(loadFavorites().saved).toBe(true);
    expect(saveFavorites('pi', ['/new']).favorites.pi).toEqual(['/new']);
    expect(saveFavorites('codex', ['/review']).saved).toBe(true);
    expect(loadFavorites().favorites).toEqual({ pi: ['/new'], codex: ['/review'], claude: [], opencode: [] });
    saveFavorites('pi', []);
    expect(loadFavorites().favorites.codex).toEqual(['/review']);
    expect(loadFavorites().favorites.pi).toEqual([]);
    expect(storage.setItem).toHaveBeenCalledWith(FAVORITES_KEY, expect.any(String));
  });
  it('reports blocked reads/writes and does not claim unsaved changes persisted', () => {
    const storage = storageFixture(JSON.stringify({ pi: ['/new'] }));
    storage.setItem.mockImplementation(() => { throw new Error('quota'); });
    expect(saveFavorites('pi', ['/model'])).toMatchObject({ saved: false, favorites: { pi: ['/new'] } });
    storage.getItem.mockImplementation(() => { throw new Error('blocked'); });
    expect(loadFavorites().saved).toBe(false);
    expect(saveFavorites('pi', ['/new']).saved).toBe(false);
  });
  it('bounds stored payloads and handles malformed JSON without executing unknown IDs', () => {
    storageFixture('not json'); expect(loadFavorites().saved).toBe(false);
    storageFixture(' '.repeat(32769)); expect(loadFavorites().saved).toBe(false);
    storageFixture(JSON.stringify({ pi: ['/obsolete', '/model\n', '/model'], opencode: ['/models'] }));
    expect(loadFavorites().favorites.pi).toEqual(['/obsolete', '/model']);
    expect(loadFavorites().favorites.opencode).toEqual(['/models']);
  });
});
