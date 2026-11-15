import { describe, expect, it } from 'vitest';
import { emptyState, type State } from './schema';
import { initialWorkspaceState, receiveWorkspaceState, usableWorkspaceMetadata } from './workspaceState';

describe('workspace metadata bootstrap and reconnect', () => {
  it('does not mistake initial, disconnected, or explicitly incomplete metadata for a loaded empty workspace', () => {
    const initial = initialWorkspaceState();
    expect(initial.initialized).toBe(false);
    for (const state of [emptyState(), { ...emptyState(), metadataReady: true }, { ...emptyState(), connected: true, metadataReady: false }]) {
      expect(usableWorkspaceMetadata(state)).toBe(false);
      expect(receiveWorkspaceState(initial, state).initialized).toBe(false);
    }
  });
  it('accepts completed empty workspaces and supports older cores without a readiness marker', () => {
    for (const state of [{ ...emptyState(), connected: true, metadataReady: true }, { ...emptyState(), connected: true }]) {
      const loaded = receiveWorkspaceState(initialWorkspaceState(), state);
      expect(loaded.initialized).toBe(true);
      expect(loaded.snapshot).toBe(state);
    }
  });
  it('can restore cached disconnected documents, but not explicitly incomplete metadata', () => {
    const snapshot: State = { ...emptyState(), metadataReady: true, documents: [{ on: null, version: 1, spaces: [] }] };
    expect(receiveWorkspaceState(initialWorkspaceState(), snapshot).initialized).toBe(true);
    expect(receiveWorkspaceState(initialWorkspaceState(), { ...snapshot, metadataReady: false }).initialized).toBe(false);
  });
  it('retains initialized layouts through stream gaps and incomplete reconnect snapshots, but accepts a genuinely empty replacement', () => {
    const snapshot: State = { ...emptyState(), connected: true, metadataReady: true, documents: [{ on: null, version: 1, spaces: [{ id: 's', name: 'Work', folder: '', tabs: [] }] }] };
    const loaded = receiveWorkspaceState(initialWorkspaceState(), snapshot);
    let current = loaded;
    for (const next of [emptyState(), { ...emptyState(), connected: true, metadataReady: false, error: 'Restoring' }]) {
      current = receiveWorkspaceState(current, next);
      expect(current.initialized).toBe(true);
      expect(current.snapshot.documents).toBe(snapshot.documents);
      expect(current.snapshot.connected).toBe(next.connected);
      expect(current.snapshot.error).toBe(next.error);
    }
    current = receiveWorkspaceState(current, { ...emptyState(), connected: true, metadataReady: true });
    expect(current.snapshot.documents).toEqual([]);
    expect(loaded.snapshot).toBe(snapshot); // No mutation of an earlier accepted state.
  });
});
