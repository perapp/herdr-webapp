import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';
import App from '../App.svelte';
import ConnectionStatus from '../ConnectionStatus.svelte';
import { emptyState } from './schema';
import { initialWorkspaceState, receiveWorkspaceState } from './workspaceState';

// Terminal widgets are never mounted by these bootstrap/empty SSR cases.
vi.mock('@xterm/xterm', () => ({ Terminal: class {} }));
vi.mock('@xterm/addon-fit', () => ({ FitAddon: class {} }));
vi.mock('@xterm/addon-web-links', () => ({ WebLinksAddon: class {} }));
vi.mock('./workspaceState', async importOriginal => {
  const original = await importOriginal<typeof import('./workspaceState')>();
  return { ...original, initialWorkspaceState: vi.fn(original.initialWorkspaceState) };
});
beforeEach(() => {
  vi.stubGlobal('location', new URL('https://app.example.test/connect/computer'));
});
afterEach(() => { vi.unstubAllGlobals(); vi.mocked(initialWorkspaceState).mockReset(); });

describe('authenticated workspace bootstrap rendering', () => {
  it('renders restoring terminals rather than onboarding or login on initial peer mount', async () => {
    const original = await vi.importActual<typeof import('./workspaceState')>('./workspaceState');
    vi.mocked(initialWorkspaceState).mockReturnValue(original.initialWorkspaceState());
    const html = render(App, { props: { peerMode: true, onConnections: () => {} } }).body;
    expect(html).toContain('Restoring your terminals');
    expect(html).toContain('data-testid="connection-workspace"');
    expect(html).not.toContain('workspace-empty');
    expect(html).not.toContain('Your Herdr workspace');
    expect(html).not.toContain('class="login-shell"');
  });
  it.each([
    { ...emptyState(), connected: true, metadataReady: false },
    { ...emptyState(), metadataReady: true },
  ])('keeps the restoring screen for incomplete or disconnected-empty snapshots: %j', snapshot => {
    vi.mocked(initialWorkspaceState).mockReturnValue(receiveWorkspaceState({ initialized: false, snapshot: emptyState() }, snapshot));
    const html = render(App, { props: { peerMode: true } }).body;
    expect(html).toContain('data-testid="connection-workspace"');
    expect(html).not.toContain('workspace-empty');
    expect(html).not.toContain('Your Herdr workspace');
  });
  it('renders genuine empty-workspace actions after completed metadata arrives', () => {
    vi.mocked(initialWorkspaceState).mockReturnValue(receiveWorkspaceState({ initialized: false, snapshot: emptyState() }, { ...emptyState(), connected: true, metadataReady: true }));
    const html = render(App, { props: { peerMode: true } }).body;
    expect(html).toContain('Your Herdr workspace');
    expect(html).toContain('workspace-empty');
    expect(html).not.toContain('data-testid="connection-workspace"');
  });
  it('does not offer a dead Connections action when no relay navigation exists', () => {
    const html = render(ConnectionStatus, { props: { name: 'Your terminals', status: 'Restoring your terminals…', retry: () => {} } }).body;
    expect(html).toContain('Retry connection');
    expect(html).not.toContain('>Connections</button>');
    expect(html).not.toContain('aria-label="Connections"');
  });
});
