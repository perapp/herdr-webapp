import { describe, expect, it } from 'vitest';
import { notificationLayoutReady, selectNotificationTarget } from './notificationTarget';
import { blankView } from './view';
import { viewKey } from './schema';
import type { SharedDocument } from './schema';
function document(on: string | null): SharedDocument {
  return { on, version: 1, spaces: [{ id: 'space', name: 'Work', folder: '/work', tabs: [{ id: 'tab', name: 'Shell', root: { id: 'stack', type: 'stack', children: [
    { id: 'first', type: 'block', content: { terminal: 't1' }, children: [] },
    { id: 'target', type: 'block', content: { terminal: 't2' }, children: [] },
    { id: 'hidden', type: 'block', hidden: true, content: { terminal: 't3' }, children: [] },
  ] } }] }] };
}
describe('authenticated notification destination resolution', () => {
  it('waits for metadata before consuming a notification hint, including an empty completed layout', () => {
    expect(notificationLayoutReady({ metadataReady: false, documents: [] })).toBe(false);
    expect(notificationLayoutReady({ metadataReady: true, documents: [] })).toBe(true);
    expect(notificationLayoutReady({ documents: [] })).toBe(false);
    expect(notificationLayoutReady({ documents: [document(null)] })).toBe(true);
    expect(notificationLayoutReady({ metadataReady: false, documents: [document(null)] })).toBe(false);
  });
  it('selects the exact machine/pane privately and reveals a stack without modifying shared layout', () => {
    const documents = [document(null), document('remote')], before = JSON.stringify(documents), view = blankView();
    view.zoomed[viewKey('remote', 'space', 'tab')] = 'first';
    expect(selectNotificationTarget(view, documents, 'remote/t2')).toBe(true);
    expect(view.space).toBe(viewKey('remote', 'space'));
    expect(view.focused[viewKey('remote', 'space', 'tab')]).toBe('target');
    expect(view.stacks[viewKey('remote', 'stack')]).toBe('target');
    expect(view.zoomed[viewKey('remote', 'space', 'tab')]).toBeUndefined();
    expect(JSON.stringify(documents)).toBe(before);
  });
  it('gracefully rejects removed, hidden, or wrong-machine targets without changing the view', () => {
    const view = blankView();
    for (const address of ['t9', 'remote/t3', 'unknown/t2']) expect(selectNotificationTarget(view, [document(null), document('remote')], address)).toBe(false);
    expect(view).toEqual(blankView());
  });
});
