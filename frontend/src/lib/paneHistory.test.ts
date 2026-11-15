import { afterEach, describe, expect, it, vi } from 'vitest';
import { agentTree } from './agents';
import type { AgentNode, AgentTarget } from './agents';
import { selectPane } from './navigation';
import { PaneHistory, createPaneHistory, forgetPaneHistory, nextReadyAgent, paneAddress, readyAgents, resolveHistoryTarget } from './paneHistory';
import type { SharedDocument, TerminalInfo } from './schema';
import { blankView } from './view';

function document(on: string | null = null): SharedDocument {
  return { on, version: 1, spaces: [{ id: 's', name: '', folder: '', tabs: [{ id: 't', name: '', root: {
    id: 'stack', type: 'stack', active: 'a', children: ['a', 'b', 'c', 'd'].map(id => ({
      id, type: 'block', children: [], content: { terminal: id },
    })),
  } }] }] };
}
function target(doc: SharedDocument, id: string): AgentTarget {
  const space = doc.spaces[0], tab = space.tabs[0];
  return { document: doc, space, tab, pane: tab.root.children.find(pane => pane.id === id)! };
}
function terminal(address: string, status = 'done', extra: Partial<TerminalInfo> = {}): TerminalInfo {
  return { address, state: 'live', status: { state: status }, ...extra };
}

describe('private pane history', () => {
  it('starts empty, deduplicates resolved observations and ignores reconnect gaps', () => {
    const history = new PaneHistory(), doc = document();
    expect(history.previous([doc])).toBeUndefined();
    expect(history.observe(undefined)).toBe(false);
    expect(history.observe(target(doc, 'a'))).toBe(true);
    expect(history.observe(target(document(), 'a'))).toBe(false);
    expect(history.observe(undefined)).toBe(false);
    expect(history.revision).toBe(1);
    expect(history.previous([doc])).toBeUndefined();
    history.observe(target(doc, 'b'));
    expect(history.previous([doc])?.pane.id).toBe('a');
  });

  it('records any observed selection, resolves fresh objects and toggles native last-pane', () => {
    const doc = document(), history = new PaneHistory();
    for (const id of ['a', 'b', 'c', 'b']) history.observe(target(doc, id));
    const latest = document(); latest.version = 2;
    const before = JSON.stringify(latest);
    const revision = history.revision;
    expect(history.previous([latest])?.pane.id).toBe('c');
    expect(history.previous([latest])?.document).toBe(latest);
    expect(history.revision).toBe(revision); // rendering availability does not navigate
    const previous = history.take([latest])!;
    expect(previous.pane.id).toBe('c');
    expect(selectPane(blankView(), previous.document, previous.space, previous.tab, previous.pane.id)).toBe(true);
    expect(history.observe(previous)).toBe(false); // parent's ensuing observation
    expect(history.take([latest])?.pane.id).toBe('b');
    expect(history.take([latest])?.pane.id).toBe('c');
    expect(JSON.stringify(latest)).toBe(before);
  });

  it('skips removed and hidden history, including hidden ancestors, but permits inactive stack leaves', () => {
    const history = new PaneHistory(), doc = document();
    for (const id of ['a', 'b', 'c', 'd']) history.observe(target(doc, id));
    const latest = document();
    latest.spaces[0].tabs[0].root.children = latest.spaces[0].tabs[0].root.children.filter(pane => pane.id !== 'c');
    latest.spaces[0].tabs[0].root.children[1].hidden = true;
    expect(history.take([latest])?.pane.id).toBe('a');
    latest.spaces[0].tabs[0].root.hidden = true;
    expect(history.previous([latest])).toBeUndefined();
    expect(history.take([])).toBeUndefined();
    expect(resolveHistoryTarget(paneAddress(target(doc, 'a')), [{ ...doc, spaces: [] }])).toBeUndefined();
  });

  it('qualifies all IDs by machine, space and tab, and keeps instances independent', () => {
    const local = document(), remote = document('remote'), history = new PaneHistory();
    history.observe(target(local, 'a'));
    history.observe(target(remote, 'a'));
    expect(history.previous([local, remote])?.document.on).toBeNull();
    const otherSpace = document(); otherSpace.spaces[0].id = 'other-space';
    const otherTab = document(); otherTab.spaces[0].tabs[0].id = 'other-tab';
    expect(resolveHistoryTarget(paneAddress(target(local, 'a')), [otherSpace])).toBeUndefined();
    expect(resolveHistoryTarget(paneAddress(target(local, 'a')), [otherTab])).toBeUndefined();
    expect(new PaneHistory().previous([local, remote])).toBeUndefined();
  });
});

describe('next unseen ready root agent', () => {
  it('counts only targetable root done using normalized seen turns, never child completion or blocked/working', () => {
    const doc = document();
    const agents = agentTree([doc], [terminal('a', 'idle', { turn: 2, labels: { 'herdr.seen': '1' } }),
      terminal('b', 'done', { turn: 2, labels: { 'herdr.seen': '2' } }), terminal('c', 'working'),
      terminal('d', 'error'), terminal('child', 'done', { parent: 'c' })]);
    expect(agents.find(agent => agent.address === 'c')?.children[0].status).toBe('idle');
    expect(readyAgents(agents).map(agent => agent.address)).toEqual(['a']);
    expect(nextReadyAgent(agents, 'a')?.address).toBe('a');
    const untargetable: AgentNode = { address: 'gone', name: '', status: 'done', children: [] };
    expect(readyAgents([untargetable])).toEqual([]);
    doc.spaces[0].tabs[0].root.children[0].hidden = true;
    expect(readyAgents(agents)).toEqual([]);
  });

  it('cycles after current in stable root order, avoids current with alternatives, and wraps', () => {
    const agents = agentTree([document()], [terminal('a'), terminal('b', 'working'), terminal('c'), terminal('d')]);
    const before = JSON.stringify(agents);
    expect(readyAgents(agents)).toHaveLength(3);
    expect(nextReadyAgent(agents, undefined)?.address).toBe('a');
    expect(nextReadyAgent(agents, 'missing')?.address).toBe('a');
    expect(nextReadyAgent(agents, 'a')?.address).toBe('c');
    expect(nextReadyAgent(agents, 'b')?.address).toBe('c');
    expect(nextReadyAgent(agents, 'c')?.address).toBe('d');
    expect(nextReadyAgent(agents, 'd')?.address).toBe('a');
    expect(nextReadyAgent([], 'a')).toBeUndefined();
    expect(JSON.stringify(agents)).toBe(before);
    expect(readyAgents([...agents, agents[0]])).toHaveLength(3);
  });

  it('does not descend into nested targetable done children, even with malformed unnormalized child status', () => {
    const [parent, child] = agentTree([document()], [terminal('a', 'working'), terminal('b')]);
    parent.children = [child];
    expect(readyAgents([parent])).toEqual([]);
    expect(nextReadyAgent([parent], undefined)).toBeUndefined();
  });
});


describe('pane history survives reconnects privately', () => {
  afterEach(() => vi.unstubAllGlobals());
  function storage() {
    const values = new Map<string, string>();
    return { values, getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); } };
  }
  it('restores last/current toggling after remount, scoped by computer, channel and tab', () => {
    const saved = storage(), doc = document();
    vi.stubGlobal('location', new URL('https://herdr.test/main/connections/computer-a'));
    const before = createPaneHistory('computer-a', saved);
    before.observe(target(doc, 'a')); before.observe(target(doc, 'b'));
    const after = createPaneHistory('computer-a', saved);
    after.observe(target(doc, 'b'));
    expect(after.take([doc])?.pane.id).toBe('a');
    expect(after.take([doc])?.pane.id).toBe('b');
    expect(createPaneHistory('computer-b', saved).previous([doc])).toBeUndefined();
    expect(createPaneHistory('computer-a', storage()).previous([doc])).toBeUndefined();
    vi.stubGlobal('location', new URL('https://herdr.test/connections/computer-a'));
    expect(createPaneHistory('computer-a', saved).previous([doc])).toBeUndefined();
  });
  it('tolerates denied/corrupt storage and skips stale restored targets', () => {
    const denied = { getItem: () => { throw Error('denied'); }, setItem: () => { throw Error('denied'); } };
    const doc = document(), fallback = createPaneHistory(null, denied);
    fallback.observe(target(doc, 'a')); fallback.observe(target(doc, 'b'));
    expect(fallback.previous([doc])?.pane.id).toBe('a');
    const saved = storage(); saved.setItem('herdr.pane-history.v1:null', '{broken');
    expect(createPaneHistory(null, saved).previous([doc])).toBeUndefined();
    saved.setItem('herdr.pane-history.v1:null', JSON.stringify([null, { pane: 42 }, { on: null, space: 's', tab: 't', pane: 'b' }, { on: null, space: 's', tab: 't', pane: 'removed' }, { on: null, space: 's', tab: 't', pane: 'a' }]));
    expect(createPaneHistory(null, saved).previous([doc])?.pane.id).toBe('a');
  });
  it('clears remembered-computer history when it is forgotten', () => {
    const saved = storage(), doc = document(); vi.stubGlobal('sessionStorage', saved);
    const history = createPaneHistory('a'); history.observe(target(doc, 'a')); history.observe(target(doc, 'b'));
    forgetPaneHistory('a');
    expect(createPaneHistory('a').previous([doc])).toBeUndefined();
  });
});
