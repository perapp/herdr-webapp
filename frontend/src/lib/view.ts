import { storageKey } from './channel';
export interface BrowserView { space: string; tabs: Record<string, string>; focused: Record<string, string>; zoomed: Record<string, string>; stacks: Record<string, string> }
const key = 'herdr.browser-view.v1';
export const blankView = (): BrowserView => ({ space: '', tabs: {}, focused: {}, zoomed: {}, stacks: {} });
export function loadView(): BrowserView {
  try {
    const v = JSON.parse(localStorage.getItem(storageKey(key)) || 'null');
    if (!v || typeof v !== 'object') return blankView();
    const strings = (input: unknown): Record<string, string> => input && typeof input === 'object' && !Array.isArray(input)
      ? Object.fromEntries(Object.entries(input).filter(([, value]) => typeof value === 'string')) : {};
    return { space: typeof v.space === 'string' ? v.space : '', tabs: strings(v.tabs), focused: strings(v.focused), zoomed: strings(v.zoomed), stacks: strings(v.stacks) };
  } catch { return blankView(); }
}
export function saveView(view: BrowserView): void { try { localStorage.setItem(storageKey(key), JSON.stringify(view)); } catch { /* Private or full storage: the current view still works. */ } }
