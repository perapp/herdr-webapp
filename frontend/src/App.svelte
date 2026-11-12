<script lang="ts">
  import { onMount } from 'svelte';
  import { MOBILE_QUERY, paneTitle, resolvePane, selectPane, visiblePanes } from './lib/navigation';
  import PaneTree from './PaneTree.svelte';
  import TopologyDialog from './TopologyDialog.svelte';
  import type { Action } from './TopologyDialog.svelte';
  import { ApiError, errorMessage, request } from './lib/api';
  import { emptyState, parseState, readable, machineName, spaceName, viewKey, findNode, record } from './lib/schema';
  import type { SharedDocument, SharedSpace, SharedTab, LayoutNode } from './lib/schema';
  import { blankView, loadView, saveView } from './lib/view';

  let { initialToken = '' }: { initialToken?: string } = $props();
  let auth = $state<'checking' | 'signed-out' | 'signed-in'>('checking');
  let token = $state('');
  let loginBusy = $state(false);
  let loggingOut = $state(false);
  let loginError = $state('');
  let snapshot = $state(emptyState());
  let streamLive = $state(false);
  let error = $state('');
  let sidebarOpen = $state(false);
  let mobile = $state(false);
  let sidebarCollapsed = $state(false);
  let showKeys = $state(false);
  let viewportHeight = $state<number>();
  let drawer = $state<HTMLDialogElement>();
  let actionsMenu = $state<HTMLDetailsElement>();
  let view = $state(blankView());
  let viewLoaded = $state(false);
  let modal = $state<{ action: Action; document: SharedDocument; machine: string; space?: string; tab?: string; pane?: string; initialName?: string } | null>(null);
  let source: EventSource | undefined;
  let lifecycle = 0;
  let revision = 0;
  let disposed = false;
  let checkingSession = false;
  const selectable = $derived(snapshot.documents.flatMap((document, i) => document.spaces.map((space, j) => ({ document, space, key: viewKey(document.on, space.id), machine: machineName(document.on, snapshot.machines, i, snapshot.core), name: spaceName(space, document.on, snapshot.terminals, `Space ${j + 1}`) }))));
  const selected = $derived(selectable.find(s => s.key === view.space) || selectable[0]);
  const spaceKey = $derived(selected?.key || '');
  const tab = $derived(selected?.space.tabs.find(t => t.id === view.tabs[spaceKey]) || selected?.space.tabs[0]);
  const tabKey = $derived(selected && tab ? viewKey(selected.document.on, selected.space.id, tab.id) : '');
  const activePane = $derived(tab ? resolvePane(tab.root, view.focused[tabKey] || '', view.stacks, selected?.document.on ?? null) : undefined);
  const focused = $derived(activePane?.id || '');
  const zoomed = $derived(tab && view.zoomed[tabKey] ? findNode(tab.root, view.zoomed[tabKey]) : undefined);
  const ready = $derived(snapshot.connected && streamLive && !loggingOut);
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
    if (checkingSession) return;
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
    source = new EventSource('/api/events');
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
  async function logout() {
    loggingOut = true; stop(); snapshot = emptyState(); modal = null;
    try { await request('/api/logout', {}); signedOut(); }
    catch { error = 'Sign-out could not be confirmed. Your streams are closed. Retry sign-out to clear the server session.'; }
    finally { loggingOut = false; }
  }
  function closeNavigation() { drawer?.close(); sidebarOpen = false; }
  function toggleNavigation() {
    if (!mobile) { sidebarCollapsed = !sidebarCollapsed; return; }
    if (!drawer) return;
    if (drawer.open) closeNavigation(); else { drawer.showModal(); sidebarOpen = true; }
  }
  function chooseSpace(key: string) { view.space = key; closeNavigation(); }
  function choosePane(document: SharedDocument, space: SharedSpace, item: SharedTab, pane: LayoutNode) {
    if (selectPane(view, document, space, item, pane.id)) closeNavigation();
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
  onMount(() => {
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
    return () => { disposed = true; stop(); media.removeEventListener('change', updateMobile); window.visualViewport?.removeEventListener('resize', updateViewport); document.removeEventListener('pointerdown', outsideActions); document.removeEventListener('keydown', escapeActions); };
  });
</script>

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
  <div class="app-shell" class:mobile class:sidebar-collapsed={sidebarCollapsed && !mobile} style:height={viewportHeight ? `${viewportHeight}px` : undefined}>
    <header class="app-header">
      <div class="app-brand">
        <button class="navigation-toggle" aria-label="Toggle navigation" aria-expanded={mobile ? sidebarOpen : !sidebarCollapsed} aria-controls={mobile ? 'navigation-drawer' : 'spaces-sidebar'} onclick={toggleNavigation}>☰</button>
        {#if !mobile}<span class="brand-name">Herdr</span>{/if}
      </div>
      {#if mobile}
        <span class="mobile-location" title={selected ? `${selected.machine} / ${selected.name} / ${readable(tab?.name, 'Workspace')} / ${activePane ? paneTitle(activePane, selected.document.on, snapshot.terminals) : ''}` : 'Herdr'}>
          <span class="dot" class:live={snapshot.connected && streamLive} aria-label={snapshot.connected && streamLive ? 'Connected' : 'Disconnected'}></span>
          {selected ? selected.name : 'Herdr'}{#if tab}<span class="location-divider">/</span>{readable(tab.name, 'Tab')}{/if}
        </span>
        <button class="keyboard-toggle" aria-label="Terminal keyboard shortcuts" aria-pressed={showKeys} title="Show terminal keys" onclick={() => showKeys = !showKeys}>⌨</button>
      {:else}
        <nav class="tab-strip" aria-label="Space tabs">
          {#each selected?.space.tabs || [] as item, i (item.id || i)}
            <button class:chosen={item.id === tab?.id} aria-current={item.id === tab?.id ? 'page' : undefined} onclick={() => view.tabs[spaceKey] = item.id} title={readable(item.name, String(i + 1))}><span class="tab-glyph" aria-hidden="true">▣</span>{readable(item.name, String(i + 1))}<small>{visiblePanes(item.root).length} {visiblePanes(item.root).length === 1 ? 'pane' : 'panes'}</small></button>
          {/each}
        </nav>
        <button class="topbar-add" aria-label="New tab" disabled={!ready || !selected} onclick={() => openAction('new-tab')}>+</button>
        <details class="workspace-menu" bind:this={actionsMenu}>
          <summary aria-label="Workspace actions" title="Workspace actions">⋯</summary>
          <div class="workspace-menu-items">{@render actions()}</div>
        </details>
      {/if}
    </header>
    {#if mobile}
      <dialog id="navigation-drawer" class="navigation-drawer" bind:this={drawer} onclose={() => sidebarOpen = false} onpointerdown={event => { if (event.target === drawer) closeNavigation(); }} aria-label="Navigate computers, workspaces, tabs and panes">
        <header class="drawer-heading"><strong>Herdr</strong><button class="quiet" aria-label="Close navigation" onclick={closeNavigation}>×</button></header>
        {@render navigation()}
        <footer class="drawer-footer"><details><summary>Workspace actions</summary><div class="drawer-actions">{@render actions()}</div></details></footer>
      </dialog>
    {:else if !sidebarCollapsed}
      <aside id="spaces-sidebar">{@render navigation()}</aside>
    {/if}
    <main class="workspace" id="workspace" aria-label={selected ? `${selected.name} terminals` : 'Terminals'}>
      {#if error || snapshot.error || !snapshot.connected}
        <div class="connection-banner" role="status"><span>{error || snapshot.error || 'The Herdr core is disconnected. Reconnecting…'}</span><button onclick={() => void start()} disabled={loggingOut}>Reconnect</button></div>
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
              <PaneTree node={zoomed || tab.root} on={selected.document.on} terminals={snapshot.terminals} {focused} stacks={view.stacks} focus={(id) => view.focused[tabKey] = id} selectStack={selectStack} />
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
{#if modal}<TopologyDialog {...modal} close={() => modal = null} saved={refresh} />{/if}

{#snippet actions()}
  <button disabled={!ready || !selected} onclick={() => action('new-tab')}>New tab</button>
  <button disabled={!ready || !selected} onclick={() => action('rename-space')}>Rename workspace</button>
  <button disabled={!ready || !tab} onclick={() => action('rename-tab')}>Rename tab</button>
  <button disabled={!ready || !focused} onclick={() => action('split')}>Split pane</button>
  {#if !mobile}<button disabled={!focused} onclick={() => { dismissActions(); view.zoomed[tabKey] = zoomed ? '' : focused; }}>{zoomed ? 'Show all panes' : 'Zoom pane'}</button>{/if}
  <button disabled={loggingOut} onclick={() => { dismissActions(); closeNavigation(); void logout(); }}>{loggingOut ? 'Signing out…' : 'Sign out'}</button>
{/snippet}

{#snippet navigation()}
  <div class="rail-heading">{mobile ? 'Computers & workspaces' : 'Spaces'}</div>
  <nav class="workspace-navigation" aria-label="Computers and workspaces">
    {#each snapshot.documents as document, i (viewKey(document.on))}
      <section class="machine-group">
        <header><span class="computer-glyph" aria-hidden="true">▱</span><h2>{machineName(document.on, snapshot.machines, i, snapshot.core)}</h2><span class="dot" class:live={snapshot.connected && streamLive && !document.error}></span><button class="icon-button" aria-label={`New workspace on ${machineName(document.on, snapshot.machines, i, snapshot.core)}`} disabled={!ready || !!document.error} onclick={() => { closeNavigation(); openAction('new-space', document); }}>+</button></header>
        {#if document.error}<p class="document-error" role="status">Layout unavailable: {document.error}</p>{/if}
        {#each document.spaces as space, j (space.id || j)}
          {@const key = viewKey(document.on, space.id)}
          {@const name = spaceName(space, document.on, snapshot.terminals, `Workspace ${j + 1}`)}
          {#if mobile}
            <details class="workspace-branch" open={spaceKey === key}>
              <summary><span class="space-badge" aria-hidden="true">{name.slice(0, 1).toUpperCase()}</span><span>{name}</span></summary>
              {#each space.tabs as item, k (item.id || k)}
                <details class="tab-branch" open={spaceKey === key && tab?.id === item.id}>
                  <summary><span aria-hidden="true">▣</span><span>{readable(item.name, String(k + 1))}</span><small>{visiblePanes(item.root).length}</small></summary>
                  <div class="pane-branches">
                    {#each visiblePanes(item.root) as pane, n (pane.id || n)}
                      <button class="pane-link" class:selected={spaceKey === key && tab?.id === item.id && focused === pane.id} aria-current={spaceKey === key && tab?.id === item.id && focused === pane.id ? 'page' : undefined} onclick={() => choosePane(document, space, item, pane)}><span class="pane-number">{n + 1}</span><span>{paneTitle(pane, document.on, snapshot.terminals, n)}</span></button>
                    {:else}<span class="rail-empty">No visible panes</span>{/each}
                  </div>
                </details>
              {:else}<button class="empty-workspace" onclick={() => chooseSpace(key)}>Open empty workspace</button>{/each}
            </details>
          {:else}
            <button class="space-button" class:selected={spaceKey === key} aria-current={spaceKey === key ? 'page' : undefined} title={space.folder || name} onclick={() => chooseSpace(key)}><span class="space-badge" aria-hidden="true">{name.slice(0, 1).toUpperCase()}</span><span>{name}</span></button>
          {/if}
        {:else}{#if !document.error}<p class="rail-empty">No workspaces yet</p>{/if}{/each}
      </section>
    {:else}<p class="rail-empty">Waiting for computer layouts…</p>{/each}
  </nav>
{/snippet}
