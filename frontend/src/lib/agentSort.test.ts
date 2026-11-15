import { describe, expect, it, vi } from 'vitest';
import { agentTree, type AgentNode } from './agents';
import { agentPinned, agentSortKey, agentUrgency, createAgentChangeTracker, groupAgents, readAgentSort, saveAgentSort } from './agentSort';
import type { SharedDocument, TerminalInfo } from './schema';

const node = (address: string, status = 'working', children: AgentNode[] = []): AgentNode => ({ address, name: address, status, children });
const terminal = (address: string, state = 'working', extra = {}): TerminalInfo => ({ address, state: 'live', status: { state }, ...extra });
const doc: SharedDocument = { on: null, version: 1, spaces: ['b', 'a'].map(id => ({ id, name: `Space ${id}`, folder: '', tabs: [{ id, name: id, root: { id, type: 'row', children: [1, 2].map(n => ({ id: `${id}${n}`, type: 'block', children: [], content: { terminal: `${id}${n}` } })) } }] })) };
function memory() {
  const values = new Map<string, string>();
  return { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}
const addresses = (groups: ReturnType<typeof groupAgents>) => groups.map(g => g.members.map(n => n.address));

describe('native agent sorting', () => {
  it('ranks urgency, then root change time, then genuine seq; ties retain provider order', () => {
    const tree = [node('idle', 'idle'), node('w1'), node('w2'), node('done', 'done'), node('blocked', 'blocked'), node('w3'), node('w4')];
    const records = [terminal('w1', 'working', { seq: 20 }), terminal('w2', 'working', { seq: 30 })];
    expect(addresses(groupAgents(tree, records, 'priority', id => id === 'w3' ? 10 : 0)))
      .toEqual([['blocked'], ['done'], ['w3', 'w2', 'w1', 'w4'], ['idle']]);
    expect(addresses(groupAgents([node('z'), node('a')], []))).toEqual([['z', 'a']]);
  });
  it('uses deepest child urgency without sorting descendants or treating child completion as news', () => {
    const kids = [node('done', 'done'), node('idle', 'idle'), node('grandparent', 'working', [node('error', 'error')])];
    const root = node('root', 'idle', kids);
    expect(agentUrgency(root)).toBe(4);
    expect(agentUrgency(node('root', 'idle', [node('done', 'done')]))).toBe(1);
    const grouped = groupAgents([node('other'), root], []);
    expect(grouped[0].members[0]).toBe(root);
    expect(root.children).toBe(kids);
    expect(root.children.map(n => n.address)).toEqual(['done', 'idle', 'grandparent']);
  });
  it('groups provider space/tab/pane order; private machine ids prevent space collisions', () => {
    const records = ['b1', 'b2', 'a1', 'a2'].map(id => terminal(id));
    const tree = agentTree([doc], records);
    expect(addresses(groupAgents(tree, records, 'grouped'))).toEqual([['b1', 'b2'], ['a1', 'a2']]);
    expect(groupAgents(tree, records, 'grouped').map(g => g.label)).toEqual(['Space b', 'Space a']);
    const remote = { ...doc, on: 'remote' };
    const both = agentTree([doc, remote], [...records, terminal('remote/b1')]);
    expect(new Set(groupAgents(both, [...records, terminal('remote/b1')], 'grouped').map(g => g.id)).size).toBe(3);
  });
  it('uses only real native pin labels, promotes kept pinned children and never alters the input', () => {
    const records = [terminal('b1'), terminal('b2', 'done', { parent: 'b1', labels: { 'herdr.pinned': '0' } }), terminal('a1', 'blocked')];
    const tree = agentTree([doc], records);
    const before = JSON.stringify(tree);
    const groups = groupAgents(tree, records);
    expect(groups[0]).toMatchObject({ id: 'pinned', label: 'Pinned' });
    expect(groups[0].members[0]).toMatchObject({ address: 'b2', status: 'done' });
    expect(tree[0].children[0].status).toBe('idle');
    expect(JSON.stringify(tree)).toBe(before);
    expect(agentPinned(terminal('x', 'idle', { pinned: true }))).toBe(false);
    expect(agentPinned(terminal('x', 'idle', { labels: { 'herdr.pinned': '' } }))).toBe(false);
  });
});

describe('private agent preferences and timestamps', () => {
  it('persists by channel and computer, safely defaults malformed values', () => {
    const storage = memory();
    expect(readAgentSort(null, storage)).toBe('priority');
    expect(saveAgentSort('remote', 'grouped', storage)).toBe(true);
    expect(readAgentSort('remote', storage)).toBe('grouped');
    expect(readAgentSort(null, storage)).toBe('priority');
    storage.setItem(agentSortKey(null), 'bad');
    expect(readAgentSort(null, storage)).toBe('priority');
    vi.stubGlobal('location', { href: 'https://example.test/main/' });
    expect(readAgentSort('remote', storage)).toBe('priority');
    expect(agentSortKey('remote')).toMatch(/^herdr.main:/);
    vi.unstubAllGlobals();
  });
  it('normalizes done/idle, persists timestamps, ignores no-pane children and tolerates reconnect gaps', () => {
    const storage = memory();
    const tracker = createAgentChangeTracker(null, storage);
    const update = (state: string, at: number) => {
      const records = [terminal('b1', state), terminal('kid', 'blocked', { parent: 'b1' })];
      tracker.update(agentTree([doc], records), records, at);
    };
    update('working', 100); expect(tracker.since('b1')).toBe(100);
    update('done', 200); expect(tracker.since('b1')).toBe(200);
    update('idle', 300); expect(tracker.since('b1')).toBe(200);
    expect(tracker.since('kid')).toBe(0);
    tracker.update([], [], 400); expect(tracker.since('b1')).toBe(200);
    expect(createAgentChangeTracker(null, storage).since('b1')).toBe(200);
    update('blocked', 500); expect(tracker.since('b1')).toBe(500);
  });
  it('survives denied reads/writes and corrupt JSON, retaining in-memory timestamps', () => {
    const denied = { getItem: () => { throw Error('denied'); }, setItem: () => { throw Error('denied'); } };
    expect(readAgentSort(null, denied)).toBe('priority');
    expect(saveAgentSort(null, 'grouped', denied)).toBe(false);
    const tracker = createAgentChangeTracker(null, denied);
    const records = [terminal('b1')];
    tracker.update(agentTree([doc], records), records, 100);
    expect(tracker.since('b1')).toBe(100);
    const broken = memory(); broken.setItem('herdr.agents.changed:null', '{');
    expect(createAgentChangeTracker(null, broken).since('b1')).toBe(0);
  });
});
