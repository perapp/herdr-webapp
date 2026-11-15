import { storageKey } from './channel';
import { connectionRoute } from './connectionRoute';
export interface BrowserView { space: string; tabs: Record<string, string>; focused: Record<string, string>; zoomed: Record<string, string>; stacks: Record<string, string> }
const key = 'herdr.browser-view.v1';
// Bind the storage identity at load time. A departing App must never save its view
// under a newly navigated URL, including when Svelte wraps the view in a proxy.
const identity = Symbol('browser-view-storage');
type BoundView = BrowserView & { [identity]?: string };
export const blankView = (): BrowserView => ({ space: '', tabs: {}, focused: {}, zoomed: {}, stacks: {} });
function viewKey(computer?: string): string {
  return storageKey(computer ? `${key}:computer:${encodeURIComponent(computer)}` : key);
}
function currentKey(): string {
  const route = typeof location === 'undefined' ? undefined : connectionRoute();
  return viewKey(route?.kind === 'computer' ? route.id : undefined);
}
export function loadView(): BrowserView {
  const savedKey = currentKey();
  let view = blankView();
  try {
    const v = JSON.parse(localStorage.getItem(savedKey) || 'null');
    if (v && typeof v === 'object') {
      const strings = (input: unknown): Record<string, string> => input && typeof input === 'object' && !Array.isArray(input)
        ? Object.fromEntries(Object.entries(input).filter(([, value]) => typeof value === 'string')) : {};
      view = { space: typeof v.space === 'string' ? v.space : '', tabs: strings(v.tabs), focused: strings(v.focused), zoomed: strings(v.zoomed), stacks: strings(v.stacks) };
    }
  } catch { /* A corrupt or unavailable saved view starts empty. */ }
  Object.defineProperty(view, identity, { value: savedKey });
  return view;
}
export function saveView(view: BrowserView): void {
  try { localStorage.setItem((view as BoundView)[identity] || currentKey(), JSON.stringify(view)); } catch { /* Private or full storage: the current view still works. */ }
}
export function forgetView(computer: string): void {
  try { localStorage.removeItem(viewKey(computer)); } catch { /* Best effort local cleanup. */ }
}
