import { describe, expect, it } from 'vitest';
import { adjacentPane, closeImpact, panePosition } from './navigation';
import type { LayoutNode, SharedSpace } from './schema';

const leaf = (id: string, hidden = false): LayoutNode => ({ id, type: 'block', hidden, content: { terminal: id }, children: [] });
const row = (id: string, ...children: LayoutNode[]): LayoutNode => ({ id, type: 'row', children });
const space = (): SharedSpace => ({ id: 's', name: 'Work', folder: '', tabs: [
  { id: 'a', name: 'A', root: row('a', leaf('a1'), leaf('a2'), leaf('gone', true)) },
  { id: 'b', name: 'B', root: row('b', leaf('b1')) },
] });

describe('swipe order', () => {
  it('walks panes across tabs in order and wraps at both ends', () => {
    const s = space();
    expect(adjacentPane(s, 'a', 'a1', 1)).toMatchObject({ tab: { id: 'a' }, pane: { id: 'a2' } });
    expect(adjacentPane(s, 'a', 'a2', 1)).toMatchObject({ tab: { id: 'b' }, pane: { id: 'b1' } });
    expect(adjacentPane(s, 'b', 'b1', 1)).toMatchObject({ tab: { id: 'a' }, pane: { id: 'a1' } });
    expect(adjacentPane(s, 'a', 'a1', -1)).toMatchObject({ tab: { id: 'b' }, pane: { id: 'b1' } });
    expect(panePosition(s, 'b', 'b1')).toEqual({ index: 3, count: 3 });
  });
  it('has nowhere to go from an unknown or only pane', () => {
    const s = space();
    expect(adjacentPane(s, 'a', 'missing', 1)).toBeUndefined();
    const single: SharedSpace = { ...s, tabs: [s.tabs[1]] };
    expect(adjacentPane(single, 'b', 'b1', 1)).toBeUndefined();
    expect(panePosition(single, 'b', 'b1')).toBeUndefined();
  });
});
describe('close impact', () => {
  it('closes only the pane while another pane remains, hidden ones included', () => {
    expect(closeImpact(space(), 'a', 'a1')).toEqual({ closes: 'pane', terminals: 1 });
  });
  it('escalates the last pane to its tab and the last tab to the workspace', () => {
    const s = space();
    expect(closeImpact(s, 'b', 'b1')).toEqual({ closes: 'tab', terminals: 1 });
    expect(closeImpact(s, 'a')).toEqual({ closes: 'tab', terminals: 3 });
    expect(closeImpact({ ...s, tabs: [s.tabs[1]] }, 'b', 'b1')).toEqual({ closes: 'space', terminals: 1 });
    expect(closeImpact(s)).toEqual({ closes: 'space', terminals: 4 });
  });
});
