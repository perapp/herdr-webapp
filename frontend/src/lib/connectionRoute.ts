import { channelBase, channelPath, storageKey } from './channel';

export type ConnectionRoute = { kind: 'connections' } | { kind: 'launch' } | { kind: 'computer'; id: string } | { kind: 'unknown' };
/** Routes contain only the public machine identifier, never pairing capabilities. */
export function connectionRoute(href = location.href): ConnectionRoute {
  const path = new URL(href).pathname;
  const base = channelBase(href);
  const local = base === '/main/' ? path.slice('/main'.length) || '/' : path;
  if (local === '/') return { kind: 'connections' };
  if (local === '/launch') return { kind: 'launch' };
  const match = /^\/connections\/([A-Za-z0-9_-]{1,128})$/.exec(local);
  return match ? { kind: 'computer', id: match[1] } : { kind: 'unknown' };
}
export function connectionPath(id: string): string {
  if (!/^[A-Za-z0-9_-]{1,128}$/.test(id)) throw new Error('Invalid computer identifier');
  return channelPath(`/connections/${id}`);
}
const selectionKey = 'herdr.last-computer.v1';
export function lastComputer(): string {
  try { return localStorage.getItem(storageKey(selectionKey)) || ''; } catch { return ''; }
}
/** Explicit selection is remembered before attempting authentication, without an expiry. */
export function rememberComputer(id: string): void {
  try { localStorage.setItem(storageKey(selectionKey), id); } catch { /* Browser storage may be unavailable. */ }
}
export function forgetSelection(id: string): void {
  try { if (lastComputer() === id) localStorage.removeItem(storageKey(selectionKey)); } catch { /* Best effort local cleanup. */ }
}
/** Invalidate before stopping: even synchronous stop callbacks belong to the old target. */
export class ConnectionGeneration {
  private generation = 0;
  invalidate(): void { this.generation++; }
  guard(): () => boolean {
    const generation = this.generation;
    return () => generation === this.generation;
  }
}
