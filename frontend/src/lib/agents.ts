import { qualify, readable, record, reportedStatus, terminalAddress } from './schema';
import { agentStatus } from './agentSeen';
import { visiblePanes } from './navigation';
import type { SharedDocument, SharedSpace, SharedTab, LayoutNode, TerminalInfo } from './schema';

export interface AgentTarget { document: SharedDocument; space: SharedSpace; tab: SharedTab; pane: LayoutNode }
export interface AgentNode { address: string; name: string; status: string; target?: AgentTarget; children: AgentNode[] }

/** Native agents() consists of reporting layout terminals; sub_agents are live parent-linked records. */
export function agentTree(documents: SharedDocument[], terminals: TerminalInfo[]): AgentNode[] {
  const targets = new Map<string, AgentTarget>();
  for (const document of documents) for (const space of document.spaces) for (const tab of space.tabs) {
    for (const pane of visiblePanes(tab.root)) if (typeof pane.content?.terminal === 'string') {
      const address = qualify(pane.content.terminal, document.on);
      if (!targets.has(address)) targets.set(address, { document, space, tab, pane });
    }
  }
  const records = new Map(terminals.filter(t => terminalAddress(t)).map(t => [terminalAddress(t), t]));
  const roots = [...targets.keys()].filter(address => records.get(address)?.state === 'live' && reportedStatus(records.get(address)) !== 'unreported');
  const children = new Map<string, string[]>();
  for (const [address, terminal] of records) {
    if (terminal.state !== 'live' || typeof terminal.parent !== 'string' || terminal.parent === address) continue;
    const siblings = children.get(terminal.parent) ?? [];
    siblings.push(address); children.set(terminal.parent, siblings);
  }
  // A kept child is shown beneath its reporting ancestor, not duplicated as a root.
  // Break malformed cycles deterministically and retain every otherwise reachable root.
  const shown = new Set<string>();
  function build(address: string, ancestors: Set<string>, nested = false): AgentNode | undefined {
    if (ancestors.has(address) || shown.has(address)) return;
    const terminal = records.get(address);
    if (!terminal) return;
    shown.add(address);
    const path = new Set(ancestors).add(address);
    const status = agentStatus(terminal);
    return {
      address, target: targets.get(address),
      name: readable(terminal.name || terminal.title || terminal.template || (record(terminal.status) ? terminal.status.app : undefined), address),
      // Children report completion to their parent, not as user-facing "Done" news.
      status: status === 'error' ? 'blocked' : nested && status === 'done' ? 'idle' : status,
      children: (children.get(address) ?? []).flatMap(child => { const node = build(child, path, true); return node ? [node] : []; }),
    };
  }
  function hasRootAncestor(address: string): boolean {
    const visited = new Set([address]);
    let parent = records.get(address)?.parent;
    while (typeof parent === 'string' && !visited.has(parent)) {
      visited.add(parent);
      if (roots.includes(parent)) return true;
      // Native descendants include only live terminals.
      if (records.get(parent)?.state !== 'live') return false;
      parent = records.get(parent)?.parent;
    }
    return false;
  }
  const ordered = [...roots.filter(address => !hasRootAncestor(address)), ...roots];
  return ordered.flatMap(address => { const node = build(address, new Set()); return node ? [node] : []; });
}
