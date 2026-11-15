import { afterEach, describe, expect, it, vi } from 'vitest';
import { MainReload, mainRevision, trackReloadActivity } from './mainReload';
const old = 'a'.repeat(40), next = 'b'.repeat(40);
const mode = (revision = old) => ({ mode: 'relay', channel: 'main', basePath: '/main/', revision, buildRevision: revision });
function setup(overrides: Partial<ConstructorParameters<typeof MainReload>[0]> = {}) {
  const values = new Map<string, string>();
  const reload = vi.fn(), changed = vi.fn();
  const fetch = vi.fn<ConstructorParameters<typeof MainReload>[0]['fetch']>(async () => ({ ok: true, json: async () => mode(next) }));
  const options = { mode: mode(), main: true, fetch, storage: { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } }, reload, changed, visible: () => true, ...overrides };
  return { updater: new MainReload(options), reload, changed, fetch, values, options };
}
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });
describe('main revision reload', () => {
  it('requires exact relay/main provenance and full matching SHA', () => {
    expect(mainRevision(mode())).toBe(old);
    expect(mainRevision({ ...mode(), buildRevision: undefined })).toBe(old);
    for (const patch of [{ mode: 'local' }, { channel: 'stable' }, { basePath: '/main' }, { revision: 'abc' }, { revision: old.toUpperCase() }, { buildRevision: next }, { revision: undefined }]) expect(mainRevision({ ...mode(), ...patch })).toBeUndefined();
  });
  it('ignores unchanged and stable/local bootstrap or path', async () => {
    for (const config of [{ mode: { ...mode(), mode: 'local' } }, { mode: { ...mode(), channel: 'stable' } }, { main: false }]) {
      const s = setup(config); await s.updater.check(); expect(s.fetch).not.toHaveBeenCalled();
    }
    const s = setup(); s.fetch.mockResolvedValue({ ok: true, json: async () => mode() });
    await s.updater.check(); expect(s.reload).not.toHaveBeenCalled();
  });
  it('offers changed deployments without automatic reload when installation is manual', async () => {
    const s = setup({ automatic: false }); await s.updater.check();
    expect(s.changed.mock.lastCall?.[0].ready).toBe(true); expect(s.reload).not.toHaveBeenCalled();
    const busy = s.updater.busy(); s.updater.manual(); expect(s.reload).not.toHaveBeenCalled();
    busy(); expect(s.reload).not.toHaveBeenCalled(); s.updater.manual(); expect(s.reload).toHaveBeenCalledOnce();
  });
  it('permits intentional stable-window reload but never during critical activity', () => {
    const s = setup({ main: false, automatic: false }), release = s.updater.busy();
    s.updater.manual(); expect(s.reload).not.toHaveBeenCalled();
    release(); s.updater.manual(); s.updater.manual(); expect(s.reload).toHaveBeenCalledOnce();
  });
  it('reloads a changed revision once with a per-target guard', async () => {
    const s = setup(); await s.updater.check(); await s.updater.check();
    expect(s.reload).toHaveBeenCalledTimes(1); expect(s.values.get(`herdr.main:reload:${next}`)).toBe('1');
  });
  it('uses compiled source, not the newer bootstrap revision', async () => {
    const s = setup({ mode: mode(next), sourceRevision: old }); await s.updater.check(); expect(s.reload).toHaveBeenCalledOnce();
  });
  it('keeps the loaded revision immutable across probes', async () => {
    const s = setup(); const key = {}; s.updater.draft(key, true);
    await s.updater.check(); s.fetch.mockResolvedValue({ ok: true, json: async () => mode('c'.repeat(40)) });
    await s.updater.check(); s.updater.draft(key, false);
    expect(s.values.has(`herdr.main:reload:${'c'.repeat(40)}`)).toBe(true);
  });
  it('defers dirty forms and latches lost drafts through disconnect', async () => {
    const s = setup(), key = {}; s.updater.draft(key, true); await s.updater.check();
    s.updater.detach(key); s.updater.draft({}, false); expect(s.reload).not.toHaveBeenCalled();
    expect(s.changed.mock.lastCall?.[0]).toEqual({ ready: true, critical: false, dirty: true });
    s.updater.manual(); expect(s.reload).toHaveBeenCalledOnce();
  });
  it('reloads after a live draft is explicitly cleared', async () => {
    const s = setup(), key = {}; s.updater.draft(key, true); await s.updater.check();
    s.updater.draft(key, false); expect(s.reload).toHaveBeenCalledOnce();
  });
  it('never allows manual or automatic reload during pairing/upload/IME', async () => {
    const s = setup(); const pairing = s.updater.busy(), upload = s.updater.busy(), ime = s.updater.busy();
    await s.updater.check(); s.updater.manual(); expect(s.reload).not.toHaveBeenCalled();
    pairing(); pairing(); upload(); s.updater.manual(); expect(s.reload).not.toHaveBeenCalled();
    ime(); expect(s.reload).toHaveBeenCalledOnce();
  });
  it('coalesces concurrent reconnect/foreground events and ignores hidden checks', async () => {
    let visible = false;
    let resolve!: (value: { ok: boolean; json(): Promise<unknown> }) => void;
    const s = setup({ visible: () => visible, fetch: vi.fn<ConstructorParameters<typeof MainReload>[0]['fetch']>(() => new Promise(r => { resolve = r; })) });
    await s.updater.check(); expect(s.options.fetch).not.toHaveBeenCalled(); visible = true;
    const pending = s.updater.check(); await s.updater.check(); expect(s.options.fetch).toHaveBeenCalledOnce();
    visible = false; resolve({ ok: true, json: async () => mode(next) }); await pending; expect(s.reload).not.toHaveBeenCalled();
  });
  it('ignores malformed responses, http and network errors', async () => {
    const s = setup();
    for (const result of [{ ok: false, json: async () => mode(next) }, { ok: true, json: async () => ({ ...mode(next), channel: 'stable' }) }]) {
      s.fetch.mockResolvedValue(result); await s.updater.check();
    }
    s.fetch.mockRejectedValue(new Error('offline')); await s.updater.check(); expect(s.reload).not.toHaveBeenCalled();
  });
  it('times out and ignores late old requests while allowing a fresh probe', async () => {
    vi.useFakeTimers();
    let resolve!: (value: { ok: boolean; json(): Promise<unknown> }) => void;
    const fetch = vi.fn<ConstructorParameters<typeof MainReload>[0]['fetch']>();
    fetch.mockImplementationOnce(() => new Promise(r => { resolve = r; }));
    fetch.mockResolvedValue({ ok: true, json: async () => mode() });
    const s = setup({ fetch }); const pending = s.updater.check();
    await vi.advanceTimersByTimeAsync(5000); expect(fetch.mock.calls[0][0].aborted).toBe(true);
    await s.updater.check(); resolve({ ok: true, json: async () => mode(next) }); await pending;
    expect(s.reload).not.toHaveBeenCalled();
  });
  it('aborts on cleanup and ignores completion and later event calls', async () => {
    let resolve!: (value: { ok: boolean; json(): Promise<unknown> }) => void;
    const s = setup({ fetch: vi.fn<ConstructorParameters<typeof MainReload>[0]['fetch']>(() => new Promise(r => { resolve = r; })) });
    const pending = s.updater.check(); s.updater.stop(); resolve({ ok: true, json: async () => mode(next) });
    await pending; await s.updater.check(); s.updater.manual(); expect(s.reload).not.toHaveBeenCalled();
    expect(s.options.fetch).toHaveBeenCalledOnce();
  });
  it('fails closed for storage failure or silently dropped writes, manual remains available', async () => {
    for (const storage of [{ getItem: () => { throw new Error('denied'); }, setItem: vi.fn() }, { getItem: () => null, setItem: vi.fn() }]) {
      const s = setup({ storage }); await s.updater.check(); expect(s.reload).not.toHaveBeenCalled();
      expect(s.changed.mock.lastCall?.[0].ready).toBe(true); s.updater.manual(); expect(s.reload).toHaveBeenCalledOnce();
    }
  });
  it('prevents stale-asset reload loops but permits explicit recovery', async () => {
    const s = setup(); s.values.set(`herdr.main:reload:${next}`, '1'); await s.updater.check();
    expect(s.reload).not.toHaveBeenCalled(); s.updater.manual(); expect(s.reload).toHaveBeenCalledOnce();
  });
});

// Minimal event/DOM facades keep activity policy tests independent of a browser engine.
class Field {
  value = ''; type = 'text'; files: unknown[] = []; isConnected = true; terminal = false;
  focus = vi.fn();
  closest() { return this.terminal ? {} : null; }
}
class Select extends Field {}
function activitySetup() {
  let mutate = () => {};
  const disconnect = vi.fn();
  vi.stubGlobal('HTMLInputElement', Field); vi.stubGlobal('HTMLSelectElement', Select);
  vi.stubGlobal('MutationObserver', class { constructor(callback: () => void) { mutate = callback; } observe() {} disconnect = disconnect; });
  const doc = new EventTarget() as EventTarget & { documentElement: object; querySelectorAll: () => Field[] };
  const fields: Field[] = [];
  doc.documentElement = {}; doc.querySelectorAll = () => fields.filter(field => field.isConnected);
  const s = setup(); const tracker = trackReloadActivity(doc as unknown as Document, s.updater);
  const emit = (type: string, field: Field) => { const event = new Event(type); Object.defineProperty(event, 'target', { value: field }); doc.dispatchEvent(event); };
  return { ...s, tracker, fields, emit, mutate: () => mutate(), disconnect };
}
describe('reload activity tracker', () => {
  it('tracks ordinary form drafts and selected files but never normal terminal input or selectors', async () => {
    const s = activitySetup(); const input = new Field(), file = new Field(), terminal = new Field(), select = new Select();
    terminal.terminal = true; terminal.value = 'already remote'; select.value = 'paired-machine';
    s.fields.push(input, file, terminal, select);
    input.value = 'unsent form'; s.emit('input', input); await s.updater.check(); expect(s.reload).not.toHaveBeenCalled();
    file.type = 'file'; file.files = [{}]; s.emit('change', file); input.value = ''; s.emit('input', input);
    expect(s.reload).not.toHaveBeenCalled(); file.files = []; s.emit('change', file); expect(s.reload).toHaveBeenCalledOnce();
    for (const field of s.fields) expect(field.focus).not.toHaveBeenCalled();
    s.tracker.stop();
  });
  it('latches drafts removed on disconnect even when a fresh empty form mounts', async () => {
    const s = activitySetup(), field = new Field(); s.fields.push(field); field.value = 'lost draft'; s.emit('input', field);
    field.isConnected = false; s.fields.push(new Field()); s.mutate(); await s.updater.check();
    expect(s.reload).not.toHaveBeenCalled(); s.updater.manual(); expect(s.reload).toHaveBeenCalledOnce(); s.tracker.stop();
  });
  it('blocks IME until compositionend without opening keyboard, focusing or replaying text', async () => {
    vi.useFakeTimers();
    const s = activitySetup(), terminal = new Field(); terminal.terminal = true; s.fields.push(terminal);
    s.emit('compositionstart', terminal); await s.updater.check(); s.updater.manual(); expect(s.reload).not.toHaveBeenCalled();
    s.emit('compositionend', terminal); expect(s.reload).not.toHaveBeenCalled();
    s.emit('input', terminal); expect(s.reload).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(0);
    expect(s.reload).toHaveBeenCalledOnce(); expect(terminal.focus).not.toHaveBeenCalled(); s.tracker.stop();
  });
  it('fails removed IME safely, latches its text, and permits only explicit discard', async () => {
    const s = activitySetup(), terminal = new Field(); terminal.terminal = true; s.fields.push(terminal);
    s.emit('compositionstart', terminal); await s.updater.check(); terminal.isConnected = false; s.mutate();
    expect(s.reload).not.toHaveBeenCalled(); expect(s.changed.mock.lastCall?.[0]).toEqual({ ready: true, critical: false, dirty: true });
    s.updater.manual(); expect(s.reload).toHaveBeenCalledOnce(); s.tracker.stop();
  });
  it('cleans event listeners, observer and queued scans on teardown', async () => {
    const s = activitySetup(), field = new Field(); s.fields.push(field); s.emit('input', field);
    s.tracker.stop(); field.value = 'late'; s.emit('input', field); s.mutate(); await Promise.resolve();
    await s.updater.check(); expect(s.reload).toHaveBeenCalledOnce(); expect(s.disconnect).toHaveBeenCalledOnce();
  });
});
