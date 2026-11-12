import { describe, expect, it } from 'vitest';
import { agentTree } from './agents';
import { selectPane } from './navigation';
import { blankView } from './view';
import { viewKey } from './schema';
import type { SharedDocument, TerminalInfo } from './schema';

function document(on: string | null = null): SharedDocument {
  return { on, version: 1, spaces: [{ id: 's', name: 'Space', folder: '', tabs: [{ id: 't', name: 'Tab', root: {
    id: 'stack', type: 'stack', active: 'parent-pane', children: ['parent', 'child', 'shell'].map(id => ({
      id: `${id}-pane`, type: 'block', content: { terminal: id }, children: [],
    })),
  } }] }] };
}
const agent = (address: string, parent?: string, state = 'working'): TerminalInfo => ({ address, parent, name: address, state: 'live', reported: true, status: { state } });

describe('native agent hierarchy', () => {
  it('roots only reporting live layout panes; nests live children even without their own status or pane', () => {
    const terminals = [agent('child', 'parent', 'done'), agent('parent'), agent('grandchild', 'child', 'error'),
      { address: 'silent', parent: 'parent', state: 'live' }, agent('outside'),
      { ...agent('finished', 'parent'), state: 'finished' }, { address: 'shell', state: 'live' }];
    const tree = agentTree([document()], terminals);
    expect(tree.map(t => t.address)).toEqual(['parent']);
    expect(tree[0].children.map(t => [t.address, t.status])).toEqual([['child', 'idle'], ['silent', 'unreported']]);
    expect(tree[0].children[0].children[0]).toMatchObject({ address: 'grandchild', status: 'blocked', target: undefined });
    expect(tree[0].children[1].target).toBeUndefined();
  });
  it('selects the exact kept child, reveals its inactive stack and clears zoom privately', () => {
    const doc = document('beta');
    const tree = agentTree([document(), doc], [agent('parent'), agent('beta/parent'), agent('beta/child', 'beta/parent')]);
    expect(tree.map(t => t.address)).toEqual(['parent', 'beta/parent']);
    const target = tree[1].children[0].target!;
    const view = blankView(); view.zoomed[viewKey('beta', 's', 't')] = 'parent-pane';
    const before = JSON.stringify(doc);
    expect(selectPane(view, target.document, target.space, target.tab, target.pane.id)).toBe(true);
    expect(view.space).toBe(viewKey('beta', 's'));
    expect(view.focused[viewKey('beta', 's', 't')]).toBe('child-pane');
    expect(view.stacks[viewKey('beta', 'stack')]).toBe('child-pane');
    expect(view.zoomed[viewKey('beta', 's', 't')]).toBeUndefined();
    expect(JSON.stringify(doc)).toBe(before);
  });
  it('does not offer hidden panes as navigation targets or show finished parents with stale status', () => {
    const doc = document(); doc.spaces[0].tabs[0].root.children[1].hidden = true;
    const tree = agentTree([doc], [agent('parent'), agent('child', 'parent')]);
    expect(tree[0].children[0].target).toBeUndefined();
    expect(agentTree([doc], [{ ...agent('parent'), state: 'finished' }])).toEqual([]);
  });
  it('handles missing ancestors, cycles, self parenting and hostile names without duplicate rows', () => {
    const tree = agentTree([document()], [agent('parent', 'child'), agent('child', 'parent'), agent('shell', 'shell')]);
    expect(tree.map(t => t.address)).toEqual(['shell', 'parent']);
    expect(tree[1].children.map(t => t.address)).toEqual(['child']);
    expect(tree[1].children[0].children).toEqual([]);
    expect(agentTree([document()], [agent('child', 'gone')])[0].address).toBe('child');
    const name = '<img src=x onerror=alert(1)>\u0007';
    expect(agentTree([document()], [{ ...agent('parent'), name }])[0].name).toBe(name.slice(0, -1));
  });
});
