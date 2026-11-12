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
  try { return parseFontSize(localStorage.getItem(fontSizeKey)); }
  catch { return defaultFontSize; }
}
export function initializeFontSize(): number {
  if (!initialized) { current = read(); initialized = true; }
  return current;
}
function publish(): void { for (const listener of listeners) listener(current); }
function storageChanged(event: StorageEvent): void {
  if (event.key !== fontSizeKey && event.key !== null) return;
  current = read(); publish();
}
export function watchFontSize(listener: (size: number) => void): () => void {
  initializeFontSize();
  if (!listeners.size) window.addEventListener('storage', storageChanged);
  listeners.add(listener); listener(current);
  return () => {
    listeners.delete(listener);
    if (!listeners.size) window.removeEventListener('storage', storageChanged);
  };
}
/** Apply immediately to all panes even if this browser cannot persist preferences. */
export function setFontSize(size: number): boolean {
  if (!valid(size)) return false;
  initializeFontSize(); current = size;
  let saved = true;
  try { localStorage.setItem(fontSizeKey, String(size)); } catch { saved = false; }
  publish(); return saved;
}
