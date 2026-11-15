import { storageKey } from './channel';
// Keep the existing per-channel key so saved terminal sizes become app-wide.
export const fontSizeKey = 'herdr.terminal-font-size';
export const defaultFontSize = 13;
export const minFontSize = 8;
export const maxFontSize = 28;
function valid(value: unknown): value is number {
  return typeof value === 'number' && Number.isInteger(value) && value >= minFontSize && value <= maxFontSize;
}
export function parseFontSize(value: unknown): number {
  const size = typeof value === 'string' && /^\d+$/.test(value) ? Number(value) : value;
  return valid(size) ? size : defaultFontSize;
}
let initialized = false;
let current = defaultFontSize;
const listeners = new Set<(size: number) => void>();
function read(): number {
  try { return parseFontSize(localStorage.getItem(storageKey(fontSizeKey))); }
  catch { return defaultFontSize; }
}
function applyUiSize(): void {
  if (typeof document !== 'undefined') document.documentElement.style.setProperty('--app-font-size', `${current}px`);
}
/** Initialize before mounting, including login/pairing screens without terminals. */
export function initializeFontSize(): number {
  if (!initialized) {
    current = read(); initialized = true;
    // App-lifetime listener: chrome still scales when no pane or Settings exists.
    window.addEventListener('storage', storageChanged);
  }
  applyUiSize();
  return current;
}
function publish(): void { applyUiSize(); for (const listener of listeners) listener(current); }
function storageChanged(event: StorageEvent): void {
  if (event.key !== storageKey(fontSizeKey) && event.key !== null) return;
  current = read(); publish();
}
export function watchFontSize(listener: (size: number) => void): () => void {
  initializeFontSize();
  listeners.add(listener); listener(current);
  return () => { listeners.delete(listener); };
}
/** Apply immediately to UI and every terminal, even when storage is blocked. */
export function setFontSize(size: number): boolean {
  if (!valid(size)) return false;
  initializeFontSize(); current = size;
  let saved = true;
  try { localStorage.setItem(storageKey(fontSizeKey), String(size)); } catch { saved = false; }
  publish(); return saved;
}
