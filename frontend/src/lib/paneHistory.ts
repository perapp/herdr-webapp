import { storageKey } from './channel';
import { record } from './schema';
import type { AgentNode, AgentTarget } from './agents';
import { visiblePanes } from './navigation';
import type { SharedDocument } from './schema';

/** Snapshot IDs, never retain an obsolete shared document or terminal address. */
export interface PaneAddress { on: string | null; space: string; tab: string; pane: string }
export function paneAddress(target: AgentTarget): PaneAddress {
  return { on: target.document.on, space: target.space.id, tab: target.tab.id, pane: target.pane.id };
}
function samePane(a: PaneAddress, b: PaneAddress): boolean {
  return a.on === b.on && a.space === b.space && a.tab === b.tab && a.pane === b.pane;
}

/** Inactive stack leaves are selectable; hidden ancestors and removed IDs are not. */
export function resolveHistoryTarget(address: PaneAddress, documents: readonly SharedDocument[]): AgentTarget | undefined {
  const document = documents.find(document => document.on === address.on);
  const space = document?.spaces.find(space => space.id === address.space);
  const tab = space?.tabs.find(tab => tab.id === address.tab);
  const pane = tab && visiblePanes(tab.root).find(pane => pane.id === address.pane);
  return document && space && tab && pane ? { document, space, tab, pane } : undefined;
}

/** Private MRU navigation. Optional tab-local storage survives workspace remounts.
 * Observe the resolved current pane, regardless of which UI action selected it.
 * Undefined (e.g. a reconnect gap) is ignored. Duplicate observations are no-ops.
 * Svelte callers can mirror revision after observe/take to invalidate previous(). */
export class PaneHistory {
  private entries: PaneAddress[] = [];
  private version = 0;
  get revision(): number { return this.version; }
  constructor(private persistence?: { key: string; storage: Pick<Storage, 'getItem' | 'setItem'> }) {
    try {
      const saved: unknown = JSON.parse(persistence?.storage.getItem(persistence.key) ?? '[]');
      if (Array.isArray(saved)) {
        const seen = new Set<string>();
        for (const entry of saved.slice(0, 64)) {
          if (!record(entry) || !(entry.on === null || typeof entry.on === 'string') ||
            !['space', 'tab', 'pane'].every(key => typeof entry[key] === 'string' && entry[key].length <= 512)) continue;
          const address = { on: entry.on, space: entry.space, tab: entry.tab, pane: entry.pane } as PaneAddress;
          const key = JSON.stringify(address);
          if (!seen.has(key)) { this.entries.push(address); seen.add(key); }
        }
      }
    } catch { /* Storage is optional; history still works in memory. */ }
  }

  observe(target: AgentTarget | undefined): boolean {
    if (!target) return false;
    const address = paneAddress(target);
    if (this.entries[0] && samePane(this.entries[0], address)) return false;
    this.entries = [address, ...this.entries.filter(entry => !samePane(entry, address))].slice(0, 64);
    try { this.persistence?.storage.setItem(this.persistence.key, JSON.stringify(this.entries)); } catch { /* In-memory fallback. */ }
    this.version++;
    return true;
  }

  /** Read-only availability lookup against the latest document snapshot. */
  previous(documents: readonly SharedDocument[]): AgentTarget | undefined {
    for (const entry of this.entries.slice(1)) {
      const target = resolveHistoryTarget(entry, documents);
      if (target) return target;
    }
  }

  /** Commit navigation in memory only; repeated calls toggle last/current pane.
   * Caller still applies selectPane; this never focuses a keyboard or sends input. */
  take(documents: readonly SharedDocument[]): AgentTarget | undefined {
    const target = this.previous(documents);
    if (target) this.observe(target);
    return target;
  }
}

function historyKey(computer: string | null): string { return storageKey(`herdr.pane-history.v1:${JSON.stringify(computer)}`); }
/** sessionStorage is private to this tab, channel and authenticated computer. */
export function createPaneHistory(computer: string | null, storage?: Pick<Storage, 'getItem' | 'setItem'>): PaneHistory {
  try { return new PaneHistory({ key: historyKey(computer), storage: storage ?? sessionStorage }); }
  catch { return new PaneHistory(); }
}
export function forgetPaneHistory(computer: string): void {
  try { sessionStorage.removeItem(historyKey(computer)); } catch { /* Optional tab-local state. */ }
}

/** Consume agentTree roots, not flattened UI groups. Their status already uses
 * agentStatus (seen turns) and normalizes nested completion to idle. No child
 * completion, blocked/working ancestor, or untargetable row is actionable news.
 * The returned array's length is the ready count; provider ordering is retained. */
export function readyAgents(agents: readonly AgentNode[]): AgentNode[] {
  const seen = new Set<string>();
  return agents.filter(agent => {
    if (agent.status !== 'done' || !agent.target || seen.has(agent.address) ||
      !resolveHistoryTarget(paneAddress(agent.target), [agent.target.document])) return false;
    seen.add(agent.address);
    return true;
  });
}

/** Cycle in stable input-root order, starting after current even if it isn't ready.
 * Current is considered last, so it is selected only when no alternative exists. */
export function nextReadyAgent(agents: readonly AgentNode[], current: string | undefined): AgentNode | undefined {
  const start = agents.findIndex(agent => agent.address === current);
  const ordered = start < 0 ? agents : [...agents.slice(start + 1), ...agents.slice(0, start + 1)];
  return readyAgents(ordered)[0];
}
