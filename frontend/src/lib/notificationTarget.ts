import { selectPane, visiblePanes } from './navigation';
import { qualify } from './schema';
import type { SharedDocument, State } from './schema';
/** Older backends lack the explicit marker; wait for at least one document there. */
export function notificationLayoutReady(state: Pick<State, 'metadataReady' | 'documents'>): boolean {
  return state.metadataReady === true || (state.metadataReady === undefined && state.documents.length > 0);
}
import type { BrowserView } from './view';
/** Routing changes private view only. It cannot focus input or send terminal data. */
export function selectNotificationTarget(view: BrowserView, documents: SharedDocument[], address: string): boolean {
  for (const document of documents) for (const space of document.spaces) for (const tab of space.tabs) {
    const pane = visiblePanes(tab.root).find(p => typeof p.content?.terminal === 'string' && qualify(p.content.terminal, document.on) === address);
    if (pane) return selectPane(view, document, space, tab, pane.id);
  }
  return false;
}
