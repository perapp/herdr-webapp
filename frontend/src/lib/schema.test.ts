import { describe, expect, it } from 'vitest';
import { parseDocument, parseState, qualify, readable, terminalName, spaceName, machineName, reportedStatus, viewKey } from './schema';

const doc = (value: unknown) => ({ on: 'laptop', version: 7, value });
describe('shared layout schema', () => {
  it('reads flattened Rust node types, stacks, weights and terminal content', () => {
    const parsed = parseDocument(doc({ spaces: [{ id: 's', name: 'Work', folder: '/work', tabs: [{ id: 't', name: 'Build', root: {
      id: 'r', type: 'row', children: [{ id: 'stack', type: 'stack', active: 'p', size: { weight: 2 }, children: [{ id: 'p', type: 'block', content: { terminal: 'shell' } }] }],
    } }] }] }));
    expect(parsed.error).toBeUndefined();
    expect(parsed.spaces[0].tabs[0].root.children[0].children[0].content?.terminal).toBe('shell');
    expect(qualify('shell', parsed.on)).toBe('laptop/shell');
  });
  it('accepts omitted fields with Rust defaults and unknown look/mod props', () => {
    const p = parseDocument(doc({ spaces: [{ id: 's', tabs: [{ id: 't' }] }] }));
    expect(p.spaces[0].name).toBe('');
    expect(p.spaces[0].tabs[0].root.children).toEqual([]);
    const mod = parseDocument(doc({ spaces: [{ tabs: [{ root: { type: 'block', content: { mod: 'viewer', props: { html: '<script>bad()</script>' } }, background: { image: 'secret' } } }] }] }));
    expect(mod.error).toBeUndefined();
  });
  it('isolates malformed machine documents and rejects malformed state envelopes', () => {
    expect(parseDocument(doc({ spaces: [{ tabs: [{ root: { type: 'javascript' } }] }] })).error).toContain('Unsupported');
    expect(() => parseState({ connected: 'yes' })).toThrow();
    expect(() => parseDocument({ on: null, version: -1, value: {} })).toThrow('version');
    const state = parseState({ connected: false, terminals: [], machines: [], documents: [doc({}), doc('bad')] });
    expect(state.documents[0].spaces).toEqual([]);
    expect(state.documents[1].error).toBeTruthy();
  });
  it('preserves explicit metadata readiness without inventing it for older servers', () => {
    const base = { connected: true, terminals: [], machines: [], documents: [] };
    expect(parseState(base).metadataReady).toBeUndefined();
    expect(parseState({ ...base, metadataReady: false }).metadataReady).toBe(false);
    expect(parseState({ ...base, metadataReady: true }).metadataReady).toBe(true);
  });
  it('bounds recursion and does not manufacture machine identity from local IDs', () => {
    let root: unknown = { type: 'block', content: {} };
    for (let i = 0; i < 55; i++) root = { type: 'row', children: [root] };
    expect(parseDocument(doc({ spaces: [{ tabs: [{ root }] }] })).error).toContain('too large');
    expect(qualify('s', null)).toBe('s');
    expect(viewKey(null, 'laptop/s')).not.toBe(viewKey('laptop', 's'));
  });
});
describe('reported terminal state', () => {
  it('uses only known states and never substitutes attached for working', () => {
    for (const state of ['working', 'done', 'blocked', 'error', 'idle']) expect(reportedStatus({ status: { state } })).toBe(state);
    expect(reportedStatus({ status: { state: '<script>bad</script>' } })).toBe('unreported');
    expect(reportedStatus(undefined)).toBe('unreported');
  });
});
describe('desktop names', () => {
  it('uses explicit names, then published checkout name, then foreground folder', () => {
    const space = parseDocument(doc({ spaces: [{ id: 's', folder: '/old/folksaga', tabs: [{ root: { type: 'block', content: { terminal: 't1' } } }] }] })).spaces[0];
    const terminal = { address: 'laptop/t1', labels: { where: 'herdr-webapp / 1' }, foreground: { cwd: '/new/subfolder' }, cwd: '/old/folksaga' };
    expect(spaceName(space, 'laptop', [terminal], 'Space 1')).toBe('herdr-webapp');
    expect(spaceName({ ...space, name: 'Named workspace' }, 'laptop', [terminal], 'Space 1')).toBe('Named workspace');
    expect(spaceName(space, 'laptop', [{ ...terminal, labels: {} }], 'Space 1')).toBe('subfolder');
    expect(machineName(null, [], 0, { name: 'perl' })).toBe('perl');
  });
});
describe('render-safe plain-text labels', () => {
  it('never treats supplied labels as markup and strips control characters', () => {
    expect(readable('<img src=x onerror=alert(1)>', 'Terminal')).toBe('<img src=x onerror=alert(1)>');
    expect(readable('a\u0000\u001bb', 'Terminal')).toBe('ab');
    expect(readable({}, 'Terminal')).toBe('Terminal');
    expect(readable('   ', 'Space')).toBe('Space');
    expect(terminalName({ id: 'raw-internal-id' })).toBe('Terminal');
  });
});
