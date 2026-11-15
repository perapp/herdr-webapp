import { describe, expect, it } from 'vitest';
import { keepPane, mountedPanes } from './keptPanes';
import type { KeptPane } from './keptPanes';
import type { LayoutNode, SharedDocument } from './schema';

const leaf = (id: string, hidden = false): LayoutNode => ({ id, type: 'block', hidden, content: { terminal: id }, children: [] });
const documents = (): SharedDocument[] => [{ on: null, version: 1, spaces: [{ id: 's', name: 'S', folder: '', tabs: [
  { id: 't', name: 'T', root: { id: 't', type: 'row', children: [leaf('a'), leaf('b'), leaf('c'), leaf('h', true)] } },
] }] }];
const at = (pane: string) => ({ on: null, space: 's', tab: 't', pane });

describe('kept phone panes', () => {
  it('keeps panes in first-shown order and refreshes recency without duplicating', () => {
    let kept: KeptPane[] = [];
    for (const pane of ['a', 'b', 'a']) kept = keepPane(kept, at(pane));
    expect(kept.map(item => item.pane)).toEqual(['a', 'b']);
    expect(kept[0].used).toBeGreaterThan(kept[1].used);
  });
  it('releases the least recently shown pane beyond the limit, never the current one', () => {
    let kept: KeptPane[] = [];
    for (const pane of ['a', 'b', 'a', 'c']) kept = keepPane(kept, at(pane), 2);
    expect(kept.map(item => item.pane)).toEqual(['a', 'c']);
  });
  it('mounts the pane on screen first, in the same pass it is selected', () => {
    const kept = keepPane(keepPane([], at('a')), at('b'));
    const mounted = mountedPanes(kept, documents(), at('c'));
    expect(mounted.map(item => [item.node.id, item.active])).toEqual([['c', true], ['a', false], ['b', false]]);
    expect(mountedPanes(kept, documents(), at('b')).map(item => item.node.id)).toEqual(['b', 'a']);
  });
  it('drops panes that were closed, hidden or belong to a missing layout', () => {
    const kept = [at('a'), at('h'), at('gone'), { ...at('a'), on: 'other' }].reduce<KeptPane[]>((list, item) => keepPane(list, item), []);
    expect(mountedPanes(kept, documents()).map(item => item.node.id)).toEqual(['a']);
    expect(mountedPanes(kept, documents(), at('gone'))).toHaveLength(1);
  });
});
