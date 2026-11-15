import type { AgentNode } from './agents';
import { agentStatus } from './agentSeen';
import { visiblePanes } from './navigation';
import { storageKey } from './channel';
import { qualify, record, terminalAddress, viewKey } from './schema';
import type { TerminalInfo } from './schema';

export type AgentSortMode = 'priority' | 'grouped';
export const agentSortOptions = [
  { mode: 'priority', label: 'Priority', icon: 'clock' },
  { mode: 'grouped', label: 'Grouped by space', icon: 'layers' },
] as const;
export interface AgentGroup { id: string; label: string; status: string | null; members: AgentNode[] }
type Storage = Pick<globalThis.Storage, 'getItem' | 'setItem'>;
export function agentSortKey(computer: string | null): string {
  return storageKey(`herdr.agents.sort:${JSON.stringify(computer)}`);
}
export function readAgentSort(computer: string | null, storage?: Storage): AgentSortMode {
  try { return (storage ?? localStorage).getItem(agentSortKey(computer)) === 'grouped' ? 'grouped' : 'priority'; }
  catch { return 'priority'; }
}
/** Private browser preference only. A failed save must not prevent updating the UI. */
export function saveAgentSort(computer: string | null, mode: AgentSortMode, storage?: Storage): boolean {
  try { (storage ?? localStorage).setItem(agentSortKey(computer), mode); return true; }
  catch { return false; }
}
const ranks: Record<string, number> = { blocked: 4, done: 3, working: 2, idle: 1 };
const normalized = (status: string) => status === 'error' ? 'blocked' : status === 'done' ? 'idle' : status;
export function agentUrgency(node: AgentNode, nested = false): number {
  return Math.max(ranks[nested ? normalized(node.status) : node.status] ?? 0,
    ...node.children.map(child => agentUrgency(child, true)));
}
export function agentPinned(terminal: TerminalInfo | undefined): boolean {
  const label = record(terminal?.labels) ? terminal.labels['herdr.pinned'] : undefined;
  // Native pin semantics: any nonempty string, including "0". Never infer pins.
  return typeof label === 'string' && label.length > 0;
}

/** Native-style local state-change times. Keep one tracker per computer; call update
 * before grouping (including empty reconnect snapshots). Done→idle never bumps.
 * Only reporting layout nodes are tracked, including kept children, as in agents(). */
export function createAgentChangeTracker(computer: string | null, storage?: Storage) {
  const key = storageKey(`herdr.agents.changed:${JSON.stringify(computer)}`);
  const changed: Record<string, { s: string; at: number }> = Object.create(null);
  try {
    const saved: unknown = JSON.parse((storage ?? localStorage).getItem(key) ?? '{}');
    if (record(saved)) for (const [address, value] of Object.entries(saved)) {
      if (record(value) && typeof value.s === 'string' && typeof value.at === 'number' && Number.isFinite(value.at) && value.at >= 0) {
        changed[address] = { s: value.s, at: value.at };
      }
    }
  } catch { /* Private browsing or damaged data: retain in-memory state. */ }
  return {
    since: (address: string): number => changed[address]?.at ?? 0,
    update(tree: readonly AgentNode[], terminals: readonly TerminalInfo[], now = Date.now()): void {
      const records = new Map(terminals.map(t => [terminalAddress(t), t]));
      const listed = new Set<string>();
      let dirty = false;
      const visit = (node: AgentNode) => {
        const terminal = records.get(node.address);
        if (node.target && terminal && agentStatus(terminal) !== 'unreported') {
          listed.add(node.address);
          const s = normalized(agentStatus(terminal));
          if (changed[node.address]?.s !== s) { changed[node.address] = { s, at: now }; dirty = true; }
        }
        node.children.forEach(visit);
      };
      tree.forEach(visit);
      if (!dirty) return;
      for (const [address, value] of Object.entries(changed)) {
        if (!listed.has(address) && now - value.at > 30 * 86400000) delete changed[address];
      }
      try { (storage ?? localStorage).setItem(key, JSON.stringify(changed)); } catch { /* Still usable in memory. */ }
    },
  };
}

/** Never reorders descendants or writes topology. Input roots retain provider
 * space/tab/pane order in grouped mode. Genuine provider seq breaks priority ties;
 * absent seq is zero, leaving stable provider order (not a fabricated core seq). */
export function groupAgents(tree: readonly AgentNode[], terminals: readonly TerminalInfo[], mode: AgentSortMode = 'priority', since: (address: string) => number = () => 0): AgentGroup[] {
  const records = new Map(terminals.map(t => [terminalAddress(t), t]));
  const roots = [...tree];
  const rootAddresses = new Set(roots.map(node => node.address));
  // A kept pinned child also has a standalone row, without removing its tree row.
  const visit = (node: AgentNode) => {
    if (!rootAddresses.has(node.address) && node.target && agentStatus(records.get(node.address)) !== 'unreported' && agentPinned(records.get(node.address))) {
      roots.push({ ...node, status: agentStatus(records.get(node.address)) }); rootAddresses.add(node.address);
    }
    node.children.forEach(visit);
  };
  tree.forEach(visit);
  const pinned = (node: AgentNode) => agentPinned(records.get(node.address));
  const seq = (node: AgentNode): number => {
    const value = records.get(node.address)?.seq;
    return typeof value === 'number' && Number.isFinite(value) ? value : 0;
  };
  if (mode === 'priority') roots.sort((a, b) => agentUrgency(b) - agentUrgency(a) || since(b.address) - since(a.address) || seq(b) - seq(a));
  const pins = roots.filter(pinned);
  if (mode === 'grouped') {
    // Promoted kept children belong in their original provider position too.
    const documents = [...new Set(roots.flatMap(node => node.target ? [node.target.document] : []))];
    const order = new Map(documents.flatMap(document => document.spaces.flatMap(space => space.tabs.flatMap(tab =>
      visiblePanes(tab.root).flatMap(pane => typeof pane.content?.terminal === 'string' ? [qualify(pane.content.terminal, document.on)] : []),
    ))).map((address, index) => [address, index]));
    pins.sort((a, b) => (order.get(a.address) ?? Infinity) - (order.get(b.address) ?? Infinity));
  }
  const groups: AgentGroup[] = pins.length ? [{ id: 'pinned', label: 'Pinned', status: null, members: pins }] : [];
  const sections: Record<number, [string, string | null]> = { 4: ['Needs you', 'blocked'], 3: ['Done', 'done'], 2: ['Working', 'working'], 1: ['Idle', null] };
  for (const node of roots.filter(node => !pinned(node))) {
    const rank = agentUrgency(node) || 1;
    const target = node.target;
    const id = mode === 'priority' ? `sec-${rank}` : `sp-${viewKey(target?.document.on ?? null, target?.space.id ?? '')}`;
    const [label, status] = mode === 'priority' ? sections[rank] : [target?.space.name ?? '', null];
    if (groups.at(-1)?.id === id) groups.at(-1)!.members.push(node);
    else groups.push({ id, label, status, members: [node] });
  }
  return groups;
}
