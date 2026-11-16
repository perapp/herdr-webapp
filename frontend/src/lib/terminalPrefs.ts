import { storageKey } from './channel';

/** Terminal behaviour a person may want to change on one device. Stored in
 * this browser only; every switch defaults to the behaviour without it.
 */
export interface TerminalPrefs {
  /** Show the key rows whenever the on-screen keyboard opens. */
  autoKeys: boolean;
  /** Keep the grid's size while the on-screen keyboard is up. */
  holdGrid: boolean;
  /** Draw terminals on the GPU instead of as page elements. */
  gpu: boolean;
}
export const terminalPrefsKey = 'herdr.terminal-prefs';
export const defaultTerminalPrefs: Readonly<TerminalPrefs> = Object.freeze({ autoKeys: true, holdGrid: true, gpu: true });
const names = Object.keys(defaultTerminalPrefs) as (keyof TerminalPrefs)[];

/** Unknown or malformed values fall back per switch, never as a whole. */
export function parseTerminalPrefs(value: unknown): TerminalPrefs {
  let stored: unknown = value;
  if (typeof value === 'string') { try { stored = JSON.parse(value); } catch { stored = undefined; } }
  const record = stored && typeof stored === 'object' && !Array.isArray(stored) ? stored as Record<string, unknown> : {};
  const prefs = { ...defaultTerminalPrefs };
  for (const name of names) if (typeof record[name] === 'boolean') prefs[name] = record[name];
  return prefs;
}
let initialized = false;
let current: TerminalPrefs = { ...defaultTerminalPrefs };
const listeners = new Set<(prefs: TerminalPrefs) => void>();
function read(): TerminalPrefs {
  try { return parseTerminalPrefs(localStorage.getItem(storageKey(terminalPrefsKey))); }
  catch { return { ...defaultTerminalPrefs }; }
}
function publish(): void { for (const listener of listeners) listener({ ...current }); }
function storageChanged(event: StorageEvent): void {
  if (event.key !== storageKey(terminalPrefsKey) && event.key !== null) return;
  current = read(); publish();
}
export function initializeTerminalPrefs(): TerminalPrefs {
  if (!initialized) {
    current = read(); initialized = true;
    if (typeof window !== 'undefined') window.addEventListener('storage', storageChanged);
  }
  return { ...current };
}
export function watchTerminalPrefs(listener: (prefs: TerminalPrefs) => void): () => void {
  initializeTerminalPrefs();
  listeners.add(listener); listener({ ...current });
  return () => { listeners.delete(listener); };
}
/** Applies at once, even when storage is blocked; returns whether it was saved. */
export function setTerminalPref(name: keyof TerminalPrefs, value: boolean): boolean {
  initializeTerminalPrefs();
  current = { ...current, [name]: value };
  let saved = true;
  try { localStorage.setItem(storageKey(terminalPrefsKey), JSON.stringify(current)); } catch { saved = false; }
  publish(); return saved;
}

/** The grid of the pane the person is looking at, for Settings to show what a
 * text size means in columns. Not persisted and not sent anywhere.
 */
export interface TerminalGrid { cols: number; rows: number }
let grid: TerminalGrid | undefined;
const gridListeners = new Set<(grid: TerminalGrid | undefined) => void>();
export function reportGrid(next: TerminalGrid | undefined): void {
  if (grid?.cols === next?.cols && grid?.rows === next?.rows) return;
  grid = next ? { cols: next.cols, rows: next.rows } : undefined;
  for (const listener of gridListeners) listener(grid);
}
export function watchGrid(listener: (grid: TerminalGrid | undefined) => void): () => void {
  gridListeners.add(listener); listener(grid);
  return () => { gridListeners.delete(listener); };
}
