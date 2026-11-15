import { visiblePanes } from './navigation';
import { viewKey } from './schema';
import type { LayoutNode, SharedDocument } from './schema';

/** Phone panes stay mounted after you leave them, so returning shows a live
 * terminal instantly: no new stream, no replay, and no channel closed on the way
 * out. Bounded: the least recently shown pane beyond the limit is released.
 */
export interface KeptPane { key: string; on: string | null; space: string; tab: string; pane: string; used: number }
export const KEPT_PANES = 6;

export function keepPane(kept: readonly KeptPane[], shown: Omit<KeptPane, 'used' | 'key'>, limit = KEPT_PANES): KeptPane[] {
  const key = viewKey(shown.on, shown.space, shown.tab, shown.pane);
  const used = kept.reduce((latest, item) => Math.max(latest, item.used), 0) + 1;
  const next = kept.some(item => item.key === key) ? kept.map(item => item.key === key ? { ...item, used } : item) : [...kept, { ...shown, key, used }];
  if (next.length <= limit) return next;
  const oldest = next.reduce((a, b) => b.used < a.used ? b : a);
  return next.filter(item => item !== oldest);
}

export interface MountedPane { key: string; on: string | null; node: LayoutNode; active: boolean }
/** Resolve kept panes against the current layout. Panes that no longer exist
 * drop out. The pane on screen is always first, kept or not, so it mounts in
 * the same frame it is selected and stays the document's first terminal.
 */
export function mountedPanes(kept: readonly KeptPane[], documents: readonly SharedDocument[], active?: Omit<KeptPane, 'used' | 'key'>): MountedPane[] {
  const activeKey = active ? viewKey(active.on, active.space, active.tab, active.pane) : '';
  const resolve = (item: Omit<KeptPane, 'used' | 'key'>, key: string): MountedPane | undefined => {
    const tab = documents.find(document => document.on === item.on)?.spaces.find(space => space.id === item.space)?.tabs.find(entry => entry.id === item.tab);
    const node = tab && visiblePanes(tab.root).find(pane => pane.id === item.pane);
    return node ? { key, on: item.on, node, active: key === activeKey } : undefined;
  };
  const shown = active && resolve(active, activeKey);
  const others = kept.filter(item => item.key !== activeKey).map(item => resolve(item, item.key)).filter((item): item is MountedPane => !!item);
  return shown ? [shown, ...others] : others;
}
