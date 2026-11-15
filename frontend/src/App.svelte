<script lang="ts">
  import { onMount } from 'svelte';
  import { transportEvents } from './lib/transport';
  import type { EventSourceLike } from './lib/transport';
  import { MOBILE_QUERY, paneTitle, resolvePane, selectCreated, selectPane, visiblePanes } from './lib/navigation';
  import { agentTree } from './lib/agents';
  import type { AgentNode } from './lib/agents';
  import PaneTree from './PaneTree.svelte';
  import TopologyDialog from './TopologyDialog.svelte';
  import SettingsDialog from './SettingsDialog.svelte';
  import NotificationPresence from './NotificationPresence.svelte';
  import { notificationLayoutReady, selectNotificationTarget } from './lib/notificationTarget';
  import InstallDialog from './InstallDialog.svelte';
  import { installApp } from './lib/pwa';
  import UpdatePanel from './UpdatePanel.svelte';
  import { initializeAppearance, watchAppearance } from './lib/appearance';
  import type { Action } from './TopologyDialog.svelte';
  import { ApiError, errorMessage, request } from './lib/api';
  import { emptyState, parseState, readable, machineName, spaceName, viewKey, findNode, record, qualify } from './lib/schema';
  import type { SharedDocument, SharedSpace, SharedTab, LayoutNode } from './lib/schema';
  import { blankView, loadView, saveView } from './lib/view';

  let { initialToken = '', peerMode = false, peerVersion = '', relayVersion = '', peerRoute = '', serverMode, peerBuild, onForget, computers = [], selectedComputer = '', onChooseComputer, onConnections }: { initialToken?: string; peerMode?: boolean; peerVersion?: string; relayVersion?: string; peerRoute?: string; serverMode?: unknown; peerBuild?: unknown; onForget?: () => void; computers?: { id: string; name: string }[]; selectedComputer?: string; onChooseComputer?: (id: string) => void; onConnections?: () => void } = $props();
  let auth = $state<'checking' | 'signed-out' | 'signed-in'>('checking');
  let token = $state('');
  let loginBusy = $state(false);
  let treeBusy = $state(false);
  let treeError = $state('');
  let expandedSpaces = $state<Record<string, boolean>>({});
  let loginError = $state('');
  let snapshot = $state(emptyState());
  let streamLive = $state(false);
  let error = $state('');
  let sidebarOpen = $state(false);
  let mobile = $state(false);
  let showKeys = $state(false);
  let showSettings = $state(false);
  let installMessage = $state('');
  async function showInstall() { installMessage = await installApp(); }
  let appearance = $state(initializeAppearance());
  let viewportHeight = $state<number>();
  let drawer = $state<HTMLDialogElement>();
  let actionsMenu = $state<HTMLDetailsElement>();
  let view = $state(blankView());
  let viewLoaded = $state(false);
  let modal = $state<{ action: Action; document: SharedDocument; machine: string; space?: string; tab?: string; pane?: string; initialName?: string } | null>(null);
  let source: EventSourceLike | undefined;
  let lifecycle = 0;
  let revision = 0;
  let disposed = false;
  let checkingSession = false;
  let notificationTarget = $state(new URLSearchParams(location.search).get('terminal') || '');
  let notificationNotice = $state('');
  const selectable = $derived(snapshot.documents.flatMap((document, i) => document.spaces.map((space, j) => ({ document, space, key: viewKey(document.on, space.id), machine: machineName(document.on, snapshot.machines, i, snapshot.core), name: spaceName(space, document.on, snapshot.terminals, `Space ${j + 1}`) }))));
  const selected = $derived(selectable.find(s => s.key === view.space) || selectable[0]);
  const spaceKey = $derived(selected?.key || '');
  const tab = $derived(selected?.space.tabs.find(t => t.id === view.tabs[spaceKey]) || selected?.space.tabs[0]);
  const tabKey = $derived(selected && tab ? viewKey(selected.document.on, selected.space.id, tab.id) : '');
  const activePane = $derived(tab ? resolvePane(tab.root, view.focused[tabKey] || '', view.stacks, selected?.document.on ?? null) : undefined);
  const focused = $derived(activePane?.id || '');
  const zoomed = $derived(tab && view.zoomed[tabKey] ? findNode(tab.root, view.zoomed[tabKey]) : undefined);
  const ready = $derived(snapshot.connected && streamLive);
  const agents = $derived(agentTree(snapshot.documents, snapshot.terminals));
  function stop() { lifecycle++; source?.close(); source = undefined; streamLive = false; }
  function signedOut() { stop(); auth = 'signed-out'; snapshot = emptyState(); modal = null; token = ''; }
  async function refresh() {
    const current = lifecycle;
    const before = revision;
    try {
      const next = parseState(await request('/api/state'));
      if (!disposed && current === lifecycle && before === revision) snapshot = next;
    } catch (e) {
      if (disposed || current !== lifecycle) return;
      if (e instanceof ApiError && e.status === 401) { signedOut(); loginError = 'Your session ended. Sign in again.'; }
      else { error = errorMessage(e); throw e; }
    }
  }
  async function checkSessionAfterDisconnect(current: number) {
    if (peerMode || checkingSession) return;
    checkingSession = true;
    try {
      const session = await request('/api/session');
      if (current === lifecycle && (!record(session) || session.authenticated !== true)) { signedOut(); loginError = 'Your session ended. Sign in again.'; }
    } catch (e) {
      if (current === lifecycle && e instanceof ApiError && e.status === 401) { signedOut(); loginError = 'Your session ended. Sign in again.'; }
    } finally { checkingSession = false; }
  }
  async function start() {
    stop();
    error = '';
    const current = lifecycle;
    // Open the stream before fetching: a newer state event wins over GET.
    source = transportEvents('/api/events');
    source.onopen = () => { if (current === lifecycle) { streamLive = true; error = ''; } };
    source.addEventListener('state', event => {
      if (disposed || current !== lifecycle) return;
      try { snapshot = parseState(JSON.parse((event as MessageEvent).data)); revision++; error = ''; streamLive = true; }
      catch (e) { error = errorMessage(e); streamLive = false; }
    });
    source.onerror = () => {
      if (current !== lifecycle || disposed) return;
      streamLive = false;
      error = 'Live updates disconnected. Reconnecting automatically; shared changes are paused.';
      void checkSessionAfterDisconnect(current);
    };
    try { await refresh(); } catch { /* Already presented above. */ }
  }
  async function login(event?: SubmitEvent, supplied?: string) {
    event?.preventDefault();
    if (loginBusy) return;
    loginBusy = true; loginError = '';
    const value = (supplied ?? token).trim();
    token = '';
    try {
      await request('/api/login', { token: value });
      if (disposed) return;
      auth = 'signed-in'; await start();
    } catch (e) {
      auth = 'signed-out';
      loginError = e instanceof ApiError ? e.message : 'Could not reach the server. Check the connection and try again.';
    } finally { loginBusy = false; }
  }
  function closeNavigation() { drawer?.close(); sidebarOpen = false; }
  function toggleNavigation() {
    if (!drawer) return;
    if (drawer.open) closeNavigation(); else { drawer.showModal(); sidebarOpen = true; }
  }
  function chooseSpace(key: string) { view.space = key; closeNavigation(); }
  function choosePane(document: SharedDocument, space: SharedSpace, item: SharedTab, pane: LayoutNode) {
    if (selectPane(view, document, space, item, pane.id)) closeNavigation();
  }
  async function topologySaved(result: unknown) {
    await refresh(); selectCreated(view, result);
  }
  function treeAction(action: Action, document: SharedDocument, space?: SharedSpace, item?: SharedTab) {
    if (!ready || treeBusy || document.error) return;
    closeNavigation();
    modal = { action, document, machine: machineName(document.on, snapshot.machines, snapshot.documents.indexOf(document), snapshot.core),
      space: space?.id, tab: item?.id,
      initialName: action === 'rename-space' ? space?.name : action === 'rename-tab' ? item?.name : '' };
  }
  async function newPane(document: SharedDocument, space: SharedSpace, item: SharedTab) {
    if (!ready || treeBusy || document.error) return;
    treeBusy = true; treeError = '';
    try {
      const result = await request('/api/topology', {action:'new-pane',on:document.on,version:document.version,space:space.id,tab:item.id});
      await topologySaved(result); closeNavigation();
    } catch (e) { treeError = `${errorMessage(e)} Refresh the layout before retrying if it changed elsewhere.`; }
    finally { treeBusy = false; }
  }
  function action(action: Action) { dismissActions(); closeNavigation(); openAction(action); }
  function selectStack(key: string, id: string) {
    view.stacks[key] = id;
    const node = tab ? findNode(tab.root, id) : undefined;
    const pane = node ? resolvePane(node, '', view.stacks, selected?.document.on ?? null) : undefined;
    if (pane) view.focused[tabKey] = pane.id;
  }
  function dismissActions() { if (actionsMenu) actionsMenu.open = false; }
  function openAction(action: Action, document = selected?.document) {
    if (!ready || !document || document.error) return;
    const pane = tab ? findNode(tab.root, focused) : undefined;
    modal = {
      action, document, machine: machineName(document.on, snapshot.machines, snapshot.documents.indexOf(document), snapshot.core),
      space: action !== 'new-space' ? selected?.space.id : undefined,
      tab: action === 'rename-tab' || action === 'split' ? tab?.id : undefined,
      pane: action === 'split' ? pane?.id : undefined,
      initialName: action === 'rename-space' ? selected?.space.name : action === 'rename-tab' ? tab?.name : '',
    };
  }
  $effect(() => { if (viewLoaded) saveView(view); });
  $effect(() => {
    if (!ready || !viewLoaded || !notificationTarget || !notificationLayoutReady(snapshot)) return;
    const found = selectNotificationTarget(view, snapshot.documents, notificationTarget);
    notificationTarget = '';
    if (!found) notificationNotice = 'This agent’s pane is no longer available. Choose a pane from the navigation menu.';
    // Consume the routing hint, not the selected computer. No input is focused.
    const url = new URL(location.href); url.searchParams.delete('terminal');
    history.replaceState(null, '', url.pathname + url.search);
  });
  onMount(() => {
    const stopAppearance = watchAppearance(value => appearance = value);
    const media = matchMedia(MOBILE_QUERY);
    const updateMobile = () => { mobile = media.matches; closeNavigation(); };
    updateMobile();
    media.addEventListener('change', updateMobile);
    const outsideActions = (event: PointerEvent) => { if (actionsMenu?.open && !actionsMenu.contains(event.target as Node)) dismissActions(); };
    const escapeActions = (event: KeyboardEvent) => { if (event.key === 'Escape') dismissActions(); };
    document.addEventListener('pointerdown', outsideActions);
    document.addEventListener('keydown', escapeActions);
    const updateViewport = () => { viewportHeight = window.visualViewport?.scale === 1 ? window.visualViewport.height : undefined; };
    updateViewport();
    window.visualViewport?.addEventListener('resize', updateViewport);
    view = loadView();
    viewLoaded = true;
    void (async () => {
      if (peerMode) { auth = 'signed-in'; await start(); return; }
      const linkToken = initialToken;
      initialToken = '';
      if (linkToken) { await login(undefined, linkToken); return; }
      try {
        const session = await request('/api/session');
        if (disposed) return;
        if (record(session) && session.authenticated === true) { auth = 'signed-in'; await start(); }
        else auth = 'signed-out';
      } catch (e) {
        if (disposed) return;
        auth = 'signed-out';
        if (!(e instanceof ApiError && e.status === 401)) loginError = 'Could not reach the server. Check the connection and try signing in.';
      }
    })();
    return () => { disposed = true; stop(); stopAppearance(); media.removeEventListener('change', updateMobile); window.visualViewport?.removeEventListener('resize', updateViewport); document.removeEventListener('pointerdown', outsideActions); document.removeEventListener('keydown', escapeActions); };
  });
</script>

<style>
  .notification-notice { position: fixed; bottom: 12px; left: 12px; right: 12px; z-index: 900; padding: 12px; background: var(--raised); border: 1px solid var(--muted); border-radius: 8px; display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .notification-notice button { min-width: 44px; min-height: 44px; }
  .peer-info { padding: 7px 10px; font-size: calc(11rem / 13); color: var(--muted); min-width: 0; }
  .peer-info summary { cursor: pointer; min-height: 28px; display: list-item; }
  .peer-info p { margin-bottom: 8px; overflow-wrap: anywhere; }
  .peer-info select { font-size: calc(11rem / 13); margin-bottom: 8px; }
  .peer-info button { width: 100%; font-size: calc(11rem / 13); overflow-wrap: anywhere; }
</style>

<svelte:head><title>{selected ? `${selected.name} · Herdr` : 'Herdr · Spaces'}</title></svelte:head>

{#if auth !== 'signed-in'}
  <main class="login-shell">
    <section class="login-card" aria-labelledby="login-heading">
      <div class="wordmark"><span class="herdr-mark" aria-hidden="true">h</span>herdr<span class="web-label">web</span></div>
      <p class="eyebrow">Your machines. One quiet workspace.</p>
      <h1 id="login-heading">Open your spaces.</h1>
      <p class="muted">Your Herdr spaces and terminals, ready to use from your phone.</p>
      {#if auth === 'checking' && !loginBusy}<p role="status">Checking your session…</p>
      {:else}
        <form onsubmit={(event) => login(event)}>
          <label for="token">Access token</label>
          <input id="token" type="password" bind:value={token} required autocomplete="off" spellcheck="false" autocapitalize="off" disabled={loginBusy} />
          {#if loginError}<p class="error" role="alert">{loginError}</p>{/if}
          <button class="primary" type="submit" disabled={loginBusy || !token.trim()}>{loginBusy ? 'Connecting…' : 'Connect to Herdr'}</button>
        </form>
      {/if}
      <p class="login-footnote">The token is exchanged for a private session cookie. It is never saved in browser storage.</p>
    </section>
  </main>
{:else}
  <div class="app-shell" class:mobile style:height={viewportHeight ? `${viewportHeight}px` : undefined}>
    <header class="app-header">
      <div class="app-brand">
        <button class="navigation-toggle" aria-label="Toggle navigation" aria-expanded={sidebarOpen} aria-controls="navigation-drawer" onclick={toggleNavigation}>☰</button>
      </div>
      <span class="mobile-location" title={selected ? `${selected.machine} / ${selected.name} / ${readable(tab?.name, 'Workspace')} / ${activePane ? paneTitle(activePane, selected.document.on, snapshot.terminals) : ''}` : 'Herdr'}>
          <span class="dot" class:live={snapshot.connected && streamLive} aria-label={snapshot.connected && streamLive ? 'Connected' : 'Disconnected'}></span>
          {selected ? selected.name : 'Herdr'}{#if tab}<span class="location-divider">/</span>{readable(tab.name, 'Tab')}{/if}
        </span>
        <button class="keyboard-toggle" aria-label="Terminal keyboard shortcuts" aria-pressed={showKeys} title="Show terminal keys" onclick={() => showKeys = !showKeys}>⌨</button>
      {#if !mobile}
        <details class="workspace-menu" bind:this={actionsMenu}>
          <summary aria-label="Workspace actions" title="Workspace actions">⋯</summary>
          <div class="workspace-menu-items">{@render actions()}</div>
        </details>
      {/if}
    </header>
    <dialog id="navigation-drawer" class="navigation-drawer" bind:this={drawer} onclose={() => sidebarOpen = false} onpointerdown={event => { if (event.target === drawer) closeNavigation(); }} aria-label="Navigate computers, workspaces, tabs and panes">
        <header class="drawer-heading"><strong>Herdr</strong><button class="quiet" aria-label="Close navigation" onclick={closeNavigation}>×</button></header>
        <div class="navigation-scroll">{@render navigation()}</div>
        {@render settingsFooter()}
    </dialog>
    <main class="workspace" id="workspace" aria-label={selected ? `${selected.name} terminals` : 'Terminals'}>
      {#if error || snapshot.error || !snapshot.connected}
        <div class="connection-banner" role="status"><span>{error || snapshot.error || 'The Herdr core is disconnected. Reconnecting…'}</span><button onclick={() => void start()}>Reconnect</button></div>
      {/if}
      {#if selected && tab}
        <div class="pane-workspace">
          {#if mobile}
            {#if activePane}
              {#key viewKey(selected.document.on, selected.space.id, tab.id, activePane.id)}
                <PaneTree node={activePane} on={selected.document.on} terminals={snapshot.terminals} {focused} stacks={view.stacks} compact showKeys={showKeys} focus={(id) => view.focused[tabKey] = id} selectStack={selectStack} />
              {/key}
            {:else}<section class="workspace-empty"><h2>No visible panes</h2><p>Choose another tab from the menu or create one.</p><button onclick={() => openAction('new-tab')}>New tab</button></section>{/if}
          {:else}
            {#key tabKey}
              <PaneTree node={zoomed || tab.root} on={selected.document.on} terminals={snapshot.terminals} {focused} stacks={view.stacks} {showKeys} focus={(id) => view.focused[tabKey] = id} selectStack={selectStack} />
            {/key}
          {/if}
        </div>
      {:else if selected}
        <section class="workspace-empty"><h2>{selected.name}</h2><p>Add a tab to open a terminal.</p><button disabled={!ready} onclick={() => openAction('new-tab')}>New tab</button></section>
      {:else}
        <section class="workspace-empty"><h2>Your Herdr workspace</h2><p>Choose or create a workspace from the menu.</p><button onclick={toggleNavigation}>Open navigation</button></section>
      {/if}
    </main>
  </div>
{/if}
{#if modal}<TopologyDialog {...modal} close={() => modal = null} saved={topologySaved} />{/if}
{#if installMessage}<InstallDialog message={installMessage} close={() => installMessage = ''} />{/if}
<NotificationPresence {ready} terminal={typeof activePane?.content?.terminal === 'string' && selected ? qualify(activePane.content.terminal, selected.document.on) : ''} />
{#if notificationNotice}<aside role="status" class="notification-notice">{notificationNotice}<button onclick={() => notificationNotice = ''} aria-label="Dismiss notification destination message">×</button></aside>{/if}
{#if showSettings}<SettingsDialog close={() => showSettings = false} {peerMode} {serverMode} {peerBuild} {peerVersion} coreVersion={snapshot.coreVersion} connected={ready} />{/if}

{#snippet settingsFooter()}
  <footer class="navigation-settings">
    <button class="settings-button" aria-label="Settings" aria-haspopup="dialog" onclick={() => showSettings = true}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><path d="m9 3-.6 2.2-2 .9-2-.6-2 3.5 1.5 1.6v2.3L2.4 14.5l2 3.5 2-.6 2 .9L9 21h4l.6-2.7 2-.9 2 .6 2-3.5-1.5-1.6v-2.3l1.5-1.6-2-3.5-2 .6-2-.9L13 3Z"/><circle cx="11" cy="12" r="3"/></svg>
      <span>Settings</span><small>{appearance.preference === 'system' ? 'System' : appearance.preference === 'dark' ? 'Dark' : 'Light'}</small>
    </button>
    <button class="settings-button" aria-haspopup="dialog" onclick={showInstall}><span>Install app</span></button>
    {#if peerMode}
      <button class="settings-button" onclick={() => { closeNavigation(); onConnections?.(); }}><span>Connections</span></button>
      <details class="peer-info"><summary>{peerRoute || 'Peer connection'}</summary>
        <p>Connector {peerVersion || 'unknown'} · Relay {relayVersion || 'unknown'}</p>
        <p>The laptop launcher manages automatic connector updates while it is running. Manual connectors must be updated on that computer; local-server update controls are not available here.</p>
        <label for="peer-computer">Computer</label>
        <select id="peer-computer" value={selectedComputer} onchange={event => onChooseComputer?.(event.currentTarget.value)}>
          {#each computers as computer (computer.id)}<option value={computer.id}>{computer.name}</option>{/each}
        </select>
        <button onclick={onForget}>Forget this computer</button>
      </details>
    {:else}<UpdatePanel />{/if}
  </footer>
{/snippet}

{#snippet actions()}
  <button disabled={!ready || !selected} onclick={() => action('new-tab')}>New tab</button>
  <button disabled={!ready || !selected} onclick={() => action('rename-space')}>Rename workspace</button>
  <button disabled={!ready || !tab} onclick={() => action('rename-tab')}>Rename tab</button>
  <button disabled={!ready || !focused} onclick={() => action('split')}>Split pane</button>
  {#if !mobile}<button disabled={!focused} onclick={() => { dismissActions(); view.zoomed[tabKey] = zoomed ? '' : focused; }}>{zoomed ? 'Show all panes' : 'Zoom pane'}</button>{/if}
{/snippet}

{#snippet navigation()}
  <div class="rail-heading">Computers &amp; workspaces</div>
  <nav class="workspace-navigation" aria-label="Computers and workspaces">
    {#each snapshot.documents as document, i (viewKey(document.on))}
      <section class="machine-group">
        <header><span class="computer-glyph" aria-hidden="true">▱</span><h2>{machineName(document.on, snapshot.machines, i, snapshot.core)}</h2><span class="dot" class:live={snapshot.connected && streamLive && !document.error}></span><button class="icon-button" aria-label={`New workspace on ${machineName(document.on, snapshot.machines, i, snapshot.core)}`} disabled={!ready || treeBusy || !!document.error} title="New workspace" onclick={() => treeAction('new-space', document)}>+</button></header>
        {#if document.error}<p class="document-error" role="status">Layout unavailable: {document.error}</p>{/if}
        {#each document.spaces as space, j (space.id || j)}
          {@const key = viewKey(document.on, space.id)}
          {@const name = spaceName(space, document.on, snapshot.terminals, `Workspace ${j + 1}`)}
            <div class="tree-node workspace-node">
              <details class="workspace-branch" open={expandedSpaces[key] ?? spaceKey === key}>
                <summary onclick={event => { event.preventDefault(); expandedSpaces[key] = !(expandedSpaces[key] ?? spaceKey === key); }}><span class="space-badge" aria-hidden="true">{name.slice(0, 1).toUpperCase()}</span><span>{name}</span></summary>
                {#each space.tabs as item, k (item.id || k)}
                  {@const tabName = readable(item.name, String(k + 1))}
                  <div class="tree-node tab-node">
                    <details class="tab-branch" open>
                      <summary><span aria-hidden="true">▣</span><span>{tabName}</span></summary>
                      <div class="pane-branches">
                        {#each visiblePanes(item.root) as pane, n (pane.id || n)}
                          <button class="pane-link" class:selected={spaceKey === key && tab?.id === item.id && focused === pane.id} aria-current={spaceKey === key && tab?.id === item.id && focused === pane.id ? 'page' : undefined} onclick={() => choosePane(document, space, item, pane)}><span class="pane-number">{n + 1}</span><span>{paneTitle(pane, document.on, snapshot.terminals, n)}</span></button>
                        {:else}<span class="rail-empty">No visible panes</span>{/each}
                      </div>
                    </details>
                    <div class="tree-node-actions">
                      <button class="tree-action" aria-label={`Rename tab ${tabName}`} title="Rename tab" disabled={!ready || treeBusy || !!document.error} onclick={() => treeAction('rename-tab', document, space, item)}>✎</button>
                      <button class="tree-action tree-create" aria-label={`New pane in ${tabName}`} title="New pane" disabled={!ready || treeBusy || !!document.error} onclick={() => void newPane(document, space, item)}>+</button>
                    </div>
                  </div>
                {:else}<button class="empty-workspace" onclick={() => chooseSpace(key)}>Open empty workspace</button>{/each}
              </details>
              <div class="tree-node-actions">
                <button class="tree-action" aria-label={`Rename workspace ${name}`} title="Rename workspace" disabled={!ready || treeBusy || !!document.error} onclick={() => treeAction('rename-space', document, space)}>✎</button>
                <button class="tree-action tree-create" aria-label={`New tab in ${name}`} title="New tab" disabled={!ready || treeBusy || !!document.error} onclick={() => treeAction('new-tab', document, space)}>+</button>
              </div>
            </div>

        {:else}{#if !document.error}<p class="rail-empty">No workspaces yet</p>{/if}{/each}
      </section>
    {:else}<p class="rail-empty">Waiting for computer layouts…</p>{/each}
  </nav>
  {#if treeError}<p class="document-error" role="alert">{treeError}</p>{/if}
  <section class="agent-section" aria-labelledby="drawer-agents-heading">
    <h2 class="rail-heading" id="drawer-agents-heading">Agents</h2>
    <nav class="agent-navigation" aria-label="Agents and sub-agents">
      {#each agents as agent (agent.address)}
        {@render agentBranch(agent)}
      {:else}<p class="rail-empty">No agents reporting status in your spaces.</p>{/each}
    </nav>
  </section>
{/snippet}

{#snippet agentBranch(agent: AgentNode)}
  {@const target = agent.target}
  {@const isSelected = target && spaceKey === viewKey(target.document.on, target.space.id) && tab?.id === target.tab.id && focused === target.pane.id}
  <div class="agent-branch">
    {#if agent.children.length}
      <details open>
        <summary aria-label={`Sub-agents of ${agent.name}`}><span class="agent-child-count">{agent.children.length}</span></summary>
        <div class="agent-children">
          {#each agent.children as child (child.address)}{@render agentBranch(child)}{/each}
        </div>
      </details>
    {/if}
    <button class="agent-link" class:has-children={agent.children.length > 0} class:selected={isSelected} aria-current={isSelected ? 'page' : undefined} disabled={!target}
      title={target ? `${agent.name} · ${readable(target.space.name, 'Workspace')} / ${readable(target.tab.name, 'Tab')}` : `${agent.name} · No visible pane. Native terminal peek is not available on the web.`}
      onclick={() => { if (target) choosePane(target.document, target.space, target.tab, target.pane); }}>
      <span class="agent-name">{agent.name}</span>
      <span class="agent-state" class:needs-you={agent.status === 'blocked'}><span class={`dot agent-${agent.status}`} aria-hidden="true"></span>{agent.status === 'blocked' ? 'Needs you' : agent.status === 'unreported' ? 'No status' : agent.status}</span>
    </button>
    {#if !target}<span class="agent-unavailable">No visible pane · peek unavailable on web</span>{/if}
  </div>
{/snippet}
