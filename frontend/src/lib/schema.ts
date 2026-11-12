export type RecordValue = Record<string, unknown>;
export interface LayoutNode {
  id: string;
  type: 'block' | 'row' | 'column' | 'stack';
  title?: string;
  hidden?: boolean;
  active?: string;
  content?: RecordValue;
  children: LayoutNode[];
  size?: { px?: number; weight?: number };
  floating?: unknown[];
}
export interface SharedTab { id: string; name: string; root: LayoutNode }
export interface SharedSpace { id: string; name: string; folder: string; tabs: SharedTab[] }
export interface SharedDocument { on: string | null; version: number; spaces: SharedSpace[]; error?: string }
export interface TerminalInfo extends RecordValue { address?: string; id?: string; terminal?: string; name?: string; title?: string; label?: string; cols?: number; rows?: number }
export interface State {
  connected: boolean;
  error?: string;
  core?: RecordValue;
  terminals: TerminalInfo[];
  machines: RecordValue[];
  documents: SharedDocument[];
}
export const emptyState = (): State => ({ connected: false, terminals: [], machines: [], documents: [] });
export function record(value: unknown): value is RecordValue {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
function string(value: unknown, fallback = ''): string { return typeof value === 'string' ? value : fallback; }
function list(value: unknown): unknown[] { if (!Array.isArray(value)) throw new Error('Expected a list in the server response.'); return value; }
function object(value: unknown): RecordValue { if (!record(value)) throw new Error('Expected an object in the server response.'); return value; }
function node(value: unknown, budget: { count: number }, depth = 0): LayoutNode {
  if (++budget.count > 4096 || depth > 48) throw new Error('This layout is too large to display safely.');
  const v = object(value);
  if (!['block', 'row', 'column', 'stack'].includes(string(v.type))) throw new Error('Unsupported layout node.');
  const type = v.type as LayoutNode['type'];
  return {
    id: string(v.id), type, title: typeof v.title === 'string' ? v.title : undefined,
    hidden: v.hidden === true, active: typeof v.active === 'string' ? v.active : undefined,
    content: record(v.content) ? v.content : undefined,
    children: type === 'block' ? [] : list(v.children ?? []).map(c => node(c, budget, depth + 1)),
    size: record(v.size) ? { px: finite(v.size.px), weight: finite(v.size.weight) } : undefined,
    floating: Array.isArray(v.floating) ? v.floating : [],
  };
}
function finite(v: unknown): number | undefined { return typeof v === 'number' && Number.isFinite(v) && v > 0 ? v : undefined; }
export function parseDocument(value: unknown): SharedDocument {
  const envelope = object(value);
  if (envelope.on !== null && typeof envelope.on !== 'string') throw new Error('Missing document machine.');
  if (!Number.isSafeInteger(envelope.version) || (envelope.version as number) < 0) throw new Error('Invalid document version.');
  const base = { on: envelope.on as string | null, version: envelope.version as number };
  if (typeof envelope.error === 'string') return { ...base, spaces: [], error: envelope.error };
  try {
    const doc = object(envelope.value);
    const budget = { count: 0 };
    const spaces = list(doc.spaces ?? []).map(value => {
      const s = object(value);
      return { id: string(s.id), name: string(s.name), folder: string(s.folder), tabs: list(s.tabs ?? []).map(value => {
        const t = object(value);
        return { id: string(t.id), name: string(t.name), root: node(t.root ?? { id: '', type: 'row', children: [] }, budget) };
      }) };
    });
    return { ...base, spaces };
  } catch (e) { return { ...base, spaces: [], error: e instanceof Error ? e.message : 'Invalid layout document.' }; }
}
export function parseState(value: unknown): State {
  const v = object(value);
  if (typeof v.connected !== 'boolean') throw new Error('Missing connection status in server response.');
  return {
    connected: v.connected, error: typeof v.error === 'string' ? v.error : undefined,
    core: record(v.core) ? v.core : undefined,
    terminals: list(v.terminals).map(v => object(v) as TerminalInfo),
    machines: list(v.machines).map(object), documents: list(v.documents).map(parseDocument),
  };
}
export function qualify(id: string, on: string | null): string { return on === null ? id : `${on}/${id}`; }
// JSON tuples avoid collisions between local and machine-qualified identities.
export function viewKey(on: string | null, ...ids: string[]): string { return JSON.stringify([on, ...ids]); }
// Plain text only: callers render this through Svelte interpolation, never HTML.
export function readable(value: unknown, fallback: string): string {
  return typeof value === 'string' && value.trim() ? [...value].filter(c => c.charCodeAt(0) > 31 && c.charCodeAt(0) !== 127).join('').slice(0, 240) || fallback : fallback;
}
export function terminalAddress(t: TerminalInfo): string { return string(t.address) || string(t.terminal) || string(t.id); }
export function reportedStatus(t: TerminalInfo | undefined): string {
  const status = record(t?.status) ? t.status.state : undefined;
  return typeof status === 'string' && ['working', 'done', 'blocked', 'error', 'idle'].includes(status) ? status : 'unreported';
}
export function terminalName(t: TerminalInfo | undefined, fallback = 'Terminal'): string {
  return readable(t?.label || t?.title || t?.name, fallback);
}
export function machineName(on: string | null, machines: RecordValue[], index: number, core?: RecordValue): string {
  const m = machines.find(m => m.id === on || m.on === on || m.name === on || m.address === on);
  return readable(on === null ? core?.name || core?.hostname : m?.label || m?.name || m?.hostname, on === null ? 'This machine' : `Remote machine ${index + 1}`);
}
function terminalNodes(root: LayoutNode): LayoutNode[] {
  return root.type === 'block' && typeof root.content?.terminal === 'string' ? [root] : root.children.flatMap(terminalNodes);
}
export function spaceName(space: SharedSpace, on: string | null, terminals: TerminalInfo[], fallback: string): string {
  if (space.name.trim()) return readable(space.name, fallback);
  const first = space.tabs.flatMap(t => terminalNodes(t.root))[0];
  const address = first && typeof first.content?.terminal === 'string' ? qualify(first.content.terminal, on) : '';
  const terminal = terminals.find(t => terminalAddress(t) === address);
  // Desktop publishes its resolved checkout/worktree name as <space> / <tab>.
  // Prefer that over a stale launch folder or a foreground process subfolder.
  const where = record(terminal?.labels) ? terminal.labels.where : undefined;
  if (typeof where === 'string' && where.includes(' / ')) return readable(where.slice(0, where.lastIndexOf(' / ')), fallback);
  const foreground = record(terminal?.foreground) ? terminal.foreground : undefined;
  const folder = typeof foreground?.cwd === 'string' ? foreground.cwd : typeof terminal?.cwd === 'string' ? terminal.cwd : space.folder;
  const basename = folder.replace(/[\\/]+$/, '').split(/[\\/]/).pop();
  return readable(basename, fallback);
}
export function findNode(root: LayoutNode, id: string): LayoutNode | undefined {
  return root.id === id ? root : root.children.map(c => findNode(c, id)).find(Boolean);
}
