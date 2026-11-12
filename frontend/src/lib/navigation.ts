import { record, qualify, readable, terminalAddress, terminalName, viewKey } from './schema';
import type { LayoutNode, SharedDocument, SharedSpace, SharedTab, TerminalInfo } from './schema';
import type { BrowserView } from './view';

export const MOBILE_QUERY = '(max-width: 600px), (max-width: 950px) and (pointer: coarse)';
/** All navigable leaves, including inactive stack children, but never hidden subtrees. */
export function visiblePanes(node: LayoutNode): LayoutNode[] {
  if (node.hidden) return [];
  return node.type === 'block' ? [node] : node.children.flatMap(visiblePanes);
}
export function resolvePane(root: LayoutNode, focused: string, stacks: Record<string, string>, on: string | null): LayoutNode | undefined {
  const existing = visiblePanes(root).find(p => p.id === focused);
  if (existing) return existing;
  function first(node: LayoutNode): LayoutNode | undefined {
    if (node.hidden) return;
    if (node.type === 'block') return typeof node.content?.terminal === 'string' ? node : undefined;
    const children = node.children.filter(c => !c.hidden);
    if (node.type === 'stack') {
      const selected = children.find(c => c.id === stacks[viewKey(on, node.id)]) || children.find(c => c.id === node.active) || children[0];
      return selected ? first(selected) : undefined;
    }
    return children.map(first).find(Boolean);
  }
  return first(root) || visiblePanes(root).find(p => typeof p.content?.terminal === 'string') || visiblePanes(root)[0];
}
export function paneTitle(pane: LayoutNode, on: string | null, terminals: TerminalInfo[], index = 0): string {
  const address = typeof pane.content?.terminal === 'string' ? qualify(pane.content.terminal, on) : '';
  const terminal = terminals.find(t => terminalAddress(t) === address);
  return readable(pane.title, address ? terminalName(terminal) : `Unavailable pane ${index + 1}`);
}
/** Navigation is private; selecting an inactive stack child also reveals it on desktop. */
export function selectPane(view: BrowserView, document: SharedDocument, space: SharedSpace, tab: SharedTab, paneId: string): boolean {
  function path(node: LayoutNode): LayoutNode[] | undefined {
    if (node.hidden) return;
    if (node.type === 'block') return node.id === paneId ? [node] : undefined;
    for (const child of node.children) {
      const found = path(child);
      if (found) return [node, ...found];
    }
  }
  const nodes = path(tab.root);
  if (!nodes) return false;
  view.space = viewKey(document.on, space.id);
  view.tabs[view.space] = tab.id;
  const tabKey = viewKey(document.on, space.id, tab.id);
  view.focused[tabKey] = paneId;
  // An old desktop zoom must not obscure the pane just selected in the drawer.
  delete view.zoomed[tabKey];
  nodes.forEach((node, i) => { if (node.type === 'stack' && nodes[i + 1]) view.stacks[viewKey(document.on, node.id)] = nodes[i + 1].id; });
  return true;
}

/** Select only confirmed creations, never fabricated IDs returned for renames. */
export function selectCreated(view: BrowserView, result: unknown): boolean {
  if (!record(result) || typeof result.terminal !== 'string' || !result.terminal ||
    !(result.on === null || typeof result.on === 'string') ||
    typeof result.space !== 'string' || typeof result.tab !== 'string' || typeof result.pane !== 'string') return false;
  view.space = viewKey(result.on, result.space);
  view.tabs[view.space] = result.tab;
  const key = viewKey(result.on, result.space, result.tab);
  view.focused[key] = result.pane;
  delete view.zoomed[key];
  return true;
}
