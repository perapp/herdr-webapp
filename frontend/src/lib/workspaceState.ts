import { emptyState, type State } from './schema';

export interface WorkspaceState { snapshot: State; initialized: boolean }
export function initialWorkspaceState(): WorkspaceState { return { snapshot: emptyState(), initialized: false }; }
/** Older cores omit metadataReady; a connected response is their completion signal. */
export function usableWorkspaceMetadata(snapshot: State): boolean {
  return snapshot.metadataReady !== false && (snapshot.connected || snapshot.documents.length > 0);
}
/** Incomplete reconnect metadata must not replace an already usable layout with onboarding. */
export function receiveWorkspaceState(previous: WorkspaceState, next: State): WorkspaceState {
  if (usableWorkspaceMetadata(next)) return { snapshot: next, initialized: true };
  return {
    initialized: previous.initialized,
    snapshot: previous.initialized ? { ...previous.snapshot, connected: next.connected, metadataReady: next.metadataReady, error: next.error } : next,
  };
}
