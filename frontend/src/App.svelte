<script lang="ts">
  import SourceLink from './SourceLink.svelte';
  import ConnectionStatus from './ConnectionStatus.svelte';
  import { initialWorkspaceState, receiveWorkspaceState } from './lib/workspaceState';
  import { onMount, tick, untrack } from 'svelte';
  import AgentSortControl from './AgentSortControl.svelte';
  import { createAgentChangeTracker, groupAgents, readAgentSort, saveAgentSort, type AgentSortMode } from './lib/agentSort';
  import { createPaneHistory, readyAgents, nextReadyAgent } from './lib/paneHistory';
  import ComputerDetailsDialog from './ComputerDetailsDialog.svelte';
  import { transportEvents } from './lib/transport';
  import type { EventSourceLike } from './lib/transport';
  import { MOBILE_QUERY, adjacentPane, closeImpact, drawnTerminals, paneTitle, panePosition, resolvePane, selectCreated, selectPane, visiblePanes } from './lib/navigation';
  import { keyboardDetector, longPress, swipe } from './lib/gestures';
  import RowMenu from './RowMenu.svelte';
  import { keepPane, mountedPanes } from './lib/keptPanes';
  import type { KeptPane } from './lib/keptPanes';
  import type { RowMenuItem } from './RowMenu.svelte';
  import { agentTree } from './lib/agents';
  import type { AgentNode } from './lib/agents';
  import PaneTree from './PaneTree.svelte';
  import TopologyDialog from './TopologyDialog.svelte';
  import SettingsDialog from './SettingsDialog.svelte';
  import NotificationPresence from './NotificationPresence.svelte';
  import AgentPresence from './AgentPresence.svelte';
  import AgentIndicator from './AgentIndicator.svelte';
  import ProgramIcon from './ProgramIcon.svelte';
  import { terminalProgram } from './lib/programs';
  import { agentStatus } from './lib/agentSeen';
  import { terminalAddress } from './lib/schema';
  import { notificationLayoutReady, selectNotificationTarget } from './lib/notificationTarget';
  import { channelPath } from './lib/channel';
  import UpdateProgress from './UpdateProgress.svelte';
  import { watchAppearance } from './lib/appearance';
  import type { Action } from './TopologyDialog.svelte';
  import { ApiError, errorMessage, request } from './lib/api';
  import { parseState, readable, machineName, spaceName, viewKey, findNode, record, qualify } from './lib/schema';
  import type { SharedDocument, SharedSpace, SharedTab, LayoutNode } from './lib/schema';
  import { blankView, loadView, saveView } from './lib/view';

  let { initialToken = '', peerMode = false, peerVersion = '', serverMode, peerBuild, computers = [], selectedComputer = '', onConnections }: { initialToken?: string; peerMode?: boolean; peerVersion?: string; serverMode?: unknown; peerBuild?: unknown; computers?: { id: string; name: string }[]; selectedComputer?: string; onConnections?: () => void } = $props();
  // RelayApp mounts us only after the authenticated peer service is ready.
  // Do not paint the local sign-in page for that first frame.
  let auth = $state<'checking' | 'signed-out' | 'signed-in'>((() => peerMode ? 'signed-in' : 'checking')());
  let token = $state('');
  let loginBusy = $state(false);
  let treeBusy = $state(false);
  let treeError = $state('');
  let expandedSpaces = $state<Record<string, boolean>>({});
  let loginError = $state('');
  let workspaceState = $state(initialWorkspaceState());
  const snapshot = $derived(workspaceState.snapshot);
  let streamLive = $state(false);
  let error = $state('');
  let sidebarOpen = $state(false);
  let mobile = $state(false);
  // Accessory keys follow the on-screen keyboard; the toggle overrides that
  // until the keyboard next opens or closes.
  let keyboardOpen = $state(false);
  let keysOverride = $state<boolean>();
  const showKeys = $derived(keysOverride ?? keyboardOpen);
  let rowMenu = $state<{ title: string; items: RowMenuItem[] } | null>(null);
  let showSettings = $state(false);
  let computerDetails = $state<{ on: string | null; name: string }>();
  let computerTrigger: HTMLButtonElement | undefined;
  let reopenNavigation = false;
  function openComputer(document: SharedDocument, index: number, event: MouseEvent) {
    computerTrigger = event.currentTarget as HTMLButtonElement; reopenNavigation = sidebarOpen;
    closeNavigation();
    computerDetails = { on: document.on, name: machineName(document.on, snapshot.machines, index, snapshot.core) };
  }
  async function closeComputer() {
    computerDetails = undefined; await tick();
    if (reopenNavigation && mobile) { toggleNavigation(); await tick(); }
    if (computerTrigger?.isConnected) computerTrigger.focus({ preventScroll: true });
  }
  let viewportHeight = $state<number>();
  let drawer = $state<HTMLDialogElement>();
  let actionsMenu = $state<HTMLDetailsElement>();
  let view = $state(blankView());
  let viewLoaded = $state(false);
  let modal = $state<{ action: Action; document: SharedDocument; machine: string; space?: string; tab?: string; pane?: string; initialName?: string; impact?: ReturnType<typeof closeImpact> } | null>(null);
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
  const activeTerminal = $derived(snapshot.terminals.find(terminal => terminalAddress(terminal) === (selected && typeof activePane?.content?.terminal === 'string' ? qualify(activePane.content.terminal, selected.document.on) : '')));
  const zoomed = $derived(tab && view.zoomed[tabKey] ? findNode(tab.root, view.zoomed[tabKey]) : undefined);
  const ready = $derived(snapshot.connected && snapshot.metadataReady !== false && streamLive);
  const agents = $derived(agentTree(snapshot.documents, snapshot.terminals));
  // App is mounted per authenticated connection; no navigation crosses computers.
  const sortComputer = untrack(() => selectedComputer || null);
  let agentSortMode = $state<AgentSortMode>(readAgentSort(sortComputer));
  const agentChanges = createAgentChangeTracker(sortComputer);
  let agentChangedTimes = $state<Record<string, number>>({});
  $effect(() => {
    agentChanges.update(agents, snapshot.terminals);
    agentChangedTimes = Object.fromEntries(snapshot.terminals.map(terminal => [terminalAddress(terminal), agentChanges.since(terminalAddress(terminal))]));
  });
  const agentGroups = $derived(groupAgents(agents, snapshot.terminals, agentSortMode, address => agentChangedTimes[address] ?? 0));
  function changeAgentSort(mode: AgentSortMode) { agentSortMode = mode; saveAgentSort(sortComputer, mode); }
  const paneHistory = createPaneHistory(sortComputer);
  let historyRevision = $state(0);
  $effect(() => {
    if (selected && tab && activePane && paneHistory.observe({ document: selected.document, space: selected.space, tab, pane: activePane })) historyRevision = paneHistory.revision;
  });
  const previousPane = $derived.by(() => { void historyRevision; return paneHistory.previous(snapshot.documents); });
  const waitingAgents = $derived(readyAgents(agents));
  function jumpReadyAgent() {
    if (!ready) return;
    const target = nextReadyAgent(agents, activeTerminal && terminalAddress(activeTerminal))?.target;
    if (target) choosePane(target.document, target.space, target.tab, target.pane);
  }
  function jumpPreviousPane() {
    if (!ready) return;
    const target = paneHistory.take(snapshot.documents);
    if (target) { historyRevision = paneHistory.revision; choosePane(target.document, target.space, target.tab, target.pane); }
  }

  function stop() { lifecycle++; source?.close(); source = undefined; streamLive = false; }
  function signedOut() { stop(); auth = 'signed-out'; workspaceState = initialWorkspaceState(); modal = null; computerDetails = undefined; token = ''; }
  async function refresh() {
    const current = lifecycle;
    const before = revision;
    try {
      const next = parseState(await request('/api/state'));
      if (!disposed && current === lifecycle && before === revision) workspaceState = receiveWorkspaceState(workspaceState, next);
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
      try { workspaceState = receiveWorkspaceState(workspaceState, parseState(JSON.parse((event as MessageEvent).data))); revision++; error = ''; streamLive = true; }
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
  function treeAction(action: Action, document: SharedDocument, space?: SharedSpace, item?: SharedTab, pane?: LayoutNode) {
    if (!ready || treeBusy || document.error) return;
    closeNavigation();
    modal = { action, document, machine: machineName(document.on, snapshot.machines, snapshot.documents.indexOf(document), snapshot.core),
      space: space?.id, tab: action === 'close-space' ? undefined : item?.id, pane: action === 'split' || action === 'close-pane' ? pane?.id : undefined,
      impact: space && action.startsWith('close-') ? closeImpact(space, action === 'close-space' ? undefined : item?.id, action === 'close-pane' ? pane?.id : undefined) : undefined,
      initialName: action === 'rename-space' ? space?.name : action === 'rename-tab' ? item?.name : '' };
  }
  // Long press (or the context-menu gesture) on a navigation row. Creation keeps
  // its visible + button; rename, split and close live here.
  function spaceMenu(document: SharedDocument, space: SharedSpace, name: string) {
    const disabled = !ready || treeBusy || !!document.error;
    rowMenu = { title: name, items: [
      { label: 'New tab', disabled, run: () => treeAction('new-tab', document, space) },
      { label: 'Rename workspace', disabled, run: () => treeAction('rename-space', document, space) },
      { label: 'Close workspace', danger: true, disabled, run: () => treeAction('close-space', document, space) },
    ] };
  }
  function tabMenu(document: SharedDocument, space: SharedSpace, item: SharedTab, name: string) {
    const disabled = !ready || treeBusy || !!document.error;
    rowMenu = { title: name, items: [
      { label: 'New pane', disabled, run: () => void newPane(document, space, item) },
      { label: 'Rename tab', disabled, run: () => treeAction('rename-tab', document, space, item) },
      { label: 'Close tab', danger: true, disabled, run: () => treeAction('close-tab', document, space, item) },
    ] };
  }
  function paneMenu(document: SharedDocument, space: SharedSpace, item: SharedTab, pane: LayoutNode, name: string) {
    const disabled = !ready || treeBusy || !!document.error;
    rowMenu = { title: name, items: [
      { label: 'Split pane', disabled, run: () => treeAction('split', document, space, item, pane) },
      { label: 'Close pane', danger: true, disabled, run: () => treeAction('close-pane', document, space, item, pane) },
    ] };
  }
  /** Phone equivalent of the desktop ⋯ menu, for the pane on screen. */
  function currentMenu() {
    if (!selected || !tab) return;
    const { document, space } = selected, pane = activePane, item = tab;
    const disabled = !ready || !!document.error;
    rowMenu = { title: `${selected.name} / ${readable(item.name, 'Tab')}`, items: [
      { label: 'New tab', disabled, run: () => treeAction('new-tab', document, space) },
      { label: 'Split pane', disabled: disabled || !pane, run: () => treeAction('split', document, space, item, pane) },
      { label: 'Rename tab', disabled, run: () => treeAction('rename-tab', document, space, item) },
      { label: 'Rename workspace', disabled, run: () => treeAction('rename-space', document, space) },
      { label: 'Close pane', danger: true, disabled: disabled || !pane, run: () => treeAction('close-pane', document, space, item, pane) },
      { label: 'Close tab', danger: true, disabled, run: () => treeAction('close-tab', document, space, item) },
    ] };
  }
  // Phones show one pane; the ones recently shown stay mounted and attached.
  let keptPanes = $state<KeptPane[]>([]);
  const shownPane = $derived(mobile && selected && tab && activePane ? { on: selected.document.on, space: selected.space.id, tab: tab.id, pane: activePane.id } : undefined);
  $effect(() => {
    const shown = shownPane;
    // Leaving phone layout releases every kept stream; desktop mounts the tab.
    untrack(() => { keptPanes = shown ? keepPane(keptPanes, shown) : mobile ? keptPanes : []; });
  });
  const phonePanes = $derived(mobile ? mountedPanes(keptPanes, snapshot.documents, shownPane) : []);
  const position = $derived(selected && tab && activePane ? panePosition(selected.space, tab.id, activePane.id) : undefined);
  /** Top-bar swipe: the next or previous pane of this workspace. Navigation only;
   * it never focuses input or opens the keyboard.
   */
  function swipePane(direction: -1 | 1) {
    if (!selected || !tab || !activePane) return;
    const next = adjacentPane(selected.space, tab.id, activePane.id, direction);
    if (next) choosePane(selected.document, selected.space, next.tab, next.pane);
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
      tab: action === 'rename-tab' || action === 'split' || action === 'close-tab' || action === 'close-pane' ? tab?.id : undefined,
      pane: action === 'split' || action === 'close-pane' ? pane?.id : undefined,
      impact: selected && action.startsWith('close-') ? closeImpact(selected.space, action === 'close-space' ? undefined : tab?.id, action === 'close-pane' ? pane?.id : undefined) : undefined,
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
    const stopAppearance = watchAppearance(() => {});
    const media = matchMedia(MOBILE_QUERY);
    const updateMobile = () => { mobile = media.matches; closeNavigation(); };
    updateMobile();
    media.addEventListener('change', updateMobile);
    const outsideActions = (event: PointerEvent) => { if (actionsMenu?.open && !actionsMenu.contains(event.target as Node)) dismissActions(); };
    const escapeActions = (event: KeyboardEvent) => { if (event.key === 'Escape') dismissActions(); };
    document.addEventListener('pointerdown', outsideActions);
    document.addEventListener('keydown', escapeActions);
    const keyboard = keyboardDetector(), coarse = matchMedia('(any-pointer: coarse)');
    const updateViewport = () => {
      const viewport = window.visualViewport;
      viewportHeight = viewport?.scale === 1 ? viewport.height : undefined;
      // Pinch zoom shrinks the visual viewport too: only an unzoomed touch device counts.
      if (!viewport || viewport.scale !== 1) return;
      const open = coarse.matches && keyboard(viewport.width, viewport.height);
      if (open !== keyboardOpen) { keyboardOpen = open; keysOverride = undefined; }
    };
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
  .agents-heading { display: flex; align-items: center; justify-content: space-between; min-height: 44px; padding-right: 10px; }
  .agents-heading .rail-heading { margin: 0; }
  .agent-group-heading { padding: 8px 14px 4px; font-size: calc(10rem / 13); font-weight: 600; color: var(--muted); overflow-wrap: anywhere; }
  .pane-jump { position: relative; display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; min-width: 36px; width: 36px; min-height: 36px; padding: 0; border: 0; background: transparent; color: var(--muted); }
  .pane-jump:not(:disabled):hover { color: var(--bright); background: var(--raised); }
  .ready-agent-count { position: absolute; top: 0; right: 0; min-width: 15px; height: 15px; padding: 0 3px; display: grid; place-items: center; border-radius: 10px; background: var(--teal); color: var(--base); font-size: 9px; font-weight: 700; line-height: 1; }
  .pane-jump:disabled .ready-agent-count { background: var(--raised); color: var(--muted); }
  @media (max-width: 760px) { .pane-jump { min-width: 44px; width: 44px; min-height: 44px; } .ready-agent-count { top: 3px; right: 2px; } }

  .computer-details-trigger { display: flex; align-items: center; gap: 7px; width: 100%; min-width: 0; min-height: 32px; border: 0; padding: 0 4px 0 0; background: transparent; text-align: left; color: var(--muted); font: inherit; }
  .computer-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  :global(.navigation-drawer) .computer-details-trigger { min-height: 44px; }
  .drawer-brand { display: inline-flex; align-items: center; min-height: 44px; min-width: 0; color: var(--bright); font-size: calc(14rem / 13); font-weight: 650; letter-spacing: -.02em; text-decoration: none; overflow-wrap: anywhere; }
  .drawer-brand:hover { color: var(--teal); }
  .drawer-brand:focus-visible { outline: 2px solid var(--teal); outline-offset: 3px; border-radius: 4px; }
  .notification-notice { position: fixed; bottom: 12px; left: 12px; right: 12px; z-index: 900; padding: 12px; background: var(--raised); border: 1px solid var(--muted); border-radius: 8px; display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .notification-notice button { min-width: 44px; min-height: 44px; }
  .kept-pane { display: flex; flex: 1; min-width: 0; min-height: 0; }
  .kept-pane[hidden] { display: none; }
  /* Horizontal swipes on the location switch panes; vertical pans stay the browser's. */
  .mobile-location { touch-action: pan-y; user-select: none; -webkit-user-select: none; -webkit-touch-callout: none; }
  .pane-position { flex-shrink: 0; margin-left: auto; padding: 0 4px; color: var(--muted); font-size: calc(10rem / 13); font-variant-numeric: tabular-nums; }
  :global(.workspace-menu-items) .danger:not(:disabled) { color: var(--error); }
  /* Long press opens row actions: no text selection or iOS callout underneath. */
  :global(.workspace-navigation) :is(summary, .pane-link) { user-select: none; -webkit-user-select: none; -webkit-touch-callout: none; }
</style>

<svelte:head><title>{selected ? `${selected.name} · Herdr` : 'Herdr · Spaces'}</title></svelte:head>

{#if auth !== 'signed-in'}
  <main class="login-shell">
    <section class="login-card" aria-labelledby="login-heading">
      <div class="wordmark"><img class="herdr-mark" src={channelPath('/icons/herdr.svg')} alt="" width="32" height="32" />herdr<span class="web-label">web</span></div>
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
      <SourceLink />
    </section>
  </main>
{:else if !workspaceState.initialized}
  <ConnectionStatus name={computers.find(c => c.id === selectedComputer)?.name || 'Your terminals'} status="Restoring your terminals…" error={error || snapshot.error} retry={() => void start()} connections={onConnections} />
{:else}
  <div class="app-shell" class:mobile style:height={viewportHeight ? `${viewportHeight}px` : undefined}>
    <header class="app-header">
      <div class="app-brand">
        <button class="navigation-toggle" aria-label="Toggle navigation" aria-expanded={sidebarOpen} aria-controls="navigation-drawer" onclick={toggleNavigation}>☰</button>
      </div>
      <span class="mobile-location" use:swipe={swipePane} use:longPress={currentMenu} title={selected ? `${selected.machine} / ${selected.name} / ${readable(tab?.name, 'Workspace')} / ${activePane ? paneTitle(activePane, selected.document.on, snapshot.terminals) : ''}` : 'Herdr'}>
          <ProgramIcon program={terminalProgram(activeTerminal)} />
          <AgentIndicator status={agentStatus(activeTerminal)} showIdle={false} />
          {selected ? selected.name : 'Herdr'}{#if tab}<span class="location-divider">/</span>{readable(tab.name, 'Tab')}{/if}
          {#if position}<span class="pane-position" aria-label={`Pane ${position.index} of ${position.count}`}>{position.index}/{position.count}</span>{/if}
        </span>
        <button class="pane-jump ready-agent-jump" aria-label={`Next unseen ready agent (${waitingAgents.length} waiting)`} title={`Next unseen ready agent — ${waitingAgents.length} waiting`} disabled={!ready || !waitingAgents.length} onclick={jumpReadyAgent}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 10 2 17v4h20v-4l-2-7 M2 17h6l2 3h4l2-3h6 M8 6l3 3 6-6"/></svg><span class="ready-agent-count" aria-hidden="true">{waitingAgents.length > 99 ? '99+' : waitingAgents.length}</span>
        </button>
        <button class="pane-jump previous-pane-jump" aria-label="Previous pane" title="Return to previous pane" disabled={!ready || !previousPane} onclick={jumpPreviousPane}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m9 4-5 5 5 5 M4 9h9a7 7 0 0 1 7 7v4"/></svg>
        </button>
        <button class="keyboard-toggle" aria-label="Terminal keyboard shortcuts" aria-pressed={showKeys} title="Show terminal keys" onclick={() => keysOverride = !showKeys}>⌨</button>
      {#if !mobile}
        <details class="workspace-menu" bind:this={actionsMenu}>
          <summary aria-label="Workspace actions" title="Workspace actions">⋯</summary>
          <div class="workspace-menu-items">{@render actions()}</div>
        </details>
      {/if}
    </header>
    <dialog id="navigation-drawer" class="navigation-drawer" bind:this={drawer} onclose={() => sidebarOpen = false} onpointerdown={event => { if (event.target === drawer) closeNavigation(); }} aria-label="Navigate computers, workspaces, tabs and panes">
        <header class="drawer-heading"><a class="drawer-brand" href={channelPath('/')} aria-label="Herdr-webapp — Connections" title="Connections" onclick={event => { if (onConnections && !event.ctrlKey && !event.metaKey && !event.shiftKey && !event.altKey && event.button === 0) { event.preventDefault(); closeNavigation(); onConnections(); } }}>Herdr-webapp</a><button class="quiet" aria-label="Close navigation" onclick={closeNavigation}>×</button></header>
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
            {#each phonePanes as kept (kept.key)}
              <div class="kept-pane" hidden={!kept.active}>
                <PaneTree node={kept.node} on={kept.on} terminals={snapshot.terminals} focused={kept.active ? focused : ''} stacks={view.stacks} compact visible={kept.active} showKeys={kept.active && showKeys} focus={(id) => { if (kept.active) view.focused[tabKey] = id; }} selectStack={selectStack} />
              </div>
            {/each}
            {#if !activePane}<section class="workspace-empty"><h2>No visible panes</h2><p>Choose another tab from the menu or create one.</p><button onclick={() => openAction('new-tab')}>New tab</button></section>{/if}
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
{#if rowMenu}<RowMenu title={rowMenu.title} items={rowMenu.items} close={() => rowMenu = null} />{/if}
<AgentPresence ready={ready && !sidebarOpen && !showSettings && !modal && !computerDetails} terminals={snapshot.terminals} visible={drawnTerminals(mobile ? activePane : zoomed || tab?.root, selected?.document.on ?? null, view.stacks)} />
<NotificationPresence {ready} terminal={typeof activePane?.content?.terminal === 'string' && selected ? qualify(activePane.content.terminal, selected.document.on) : ''} />
{#if notificationNotice}<aside role="status" class="notification-notice">{notificationNotice}<button onclick={() => notificationNotice = ''} aria-label="Dismiss notification destination message">×</button></aside>{/if}
{#if !peerMode}<UpdateProgress connected={ready} />{/if}
{#if computerDetails}<ComputerDetailsDialog name={computerDetails.name} on={computerDetails.on} document={snapshot.documents.find(doc => doc.on === computerDetails?.on)} connected={ready} coreVersion={snapshot.coreVersion} refresh={refresh} close={() => void closeComputer()} />{/if}
{#if showSettings}<SettingsDialog close={() => showSettings = false} {peerMode} {serverMode} {peerBuild} {peerVersion} coreVersion={snapshot.coreVersion} connected={ready} computerName={computers.find(c => c.id === selectedComputer)?.name || machineName(null, snapshot.machines, 0, snapshot.core)} />{/if}

{#snippet settingsFooter()}
  <footer class="navigation-settings">
    <button class="settings-button" aria-label="Settings" title="Settings" aria-haspopup="dialog" onclick={() => showSettings = true}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><path d="m9 3-.6 2.2-2 .9-2-.6-2 3.5 1.5 1.6v2.3L2.4 14.5l2 3.5 2-.6 2 .9L9 21h4l.6-2.7 2-.9 2 .6 2-3.5-1.5-1.6v-2.3l1.5-1.6-2-3.5-2 .6-2-.9L13 3Z"/><circle cx="11" cy="12" r="3"/></svg>
    </button>
  </footer>
{/snippet}

{#snippet actions()}
  <button disabled={!ready || !selected} onclick={() => action('new-tab')}>New tab</button>
  <button disabled={!ready || !selected} onclick={() => action('rename-space')}>Rename workspace</button>
  <button disabled={!ready || !tab} onclick={() => action('rename-tab')}>Rename tab</button>
  <button disabled={!ready || !focused} onclick={() => action('split')}>Split pane</button>
  <button class="danger" disabled={!ready || !focused} onclick={() => action('close-pane')}>Close pane</button>
  <button class="danger" disabled={!ready || !tab} onclick={() => action('close-tab')}>Close tab</button>
  <button class="danger" disabled={!ready || !selected} onclick={() => action('close-space')}>Close workspace</button>
  {#if !mobile}<button disabled={!focused} onclick={() => { dismissActions(); view.zoomed[tabKey] = zoomed ? '' : focused; }}>{zoomed ? 'Show all panes' : 'Zoom pane'}</button>{/if}
{/snippet}

{#snippet navigation()}
  <div class="rail-heading">Computers &amp; workspaces</div>
  <nav class="workspace-navigation" aria-label="Computers and workspaces">
    {#each snapshot.documents as document, i (viewKey(document.on))}
      <section class="machine-group">
        <header><h2><button class="computer-details-trigger" aria-label={`Computer details: ${machineName(document.on, snapshot.machines, i, snapshot.core)}`} aria-haspopup="dialog" onclick={event => openComputer(document, i, event)}><span class="computer-glyph" aria-hidden="true">▱</span><span class="computer-name">{machineName(document.on, snapshot.machines, i, snapshot.core)}</span><span class="dot" class:live={snapshot.connected && streamLive && !document.error} aria-label={snapshot.connected && streamLive && !document.error ? 'Connected' : 'Unavailable'}></span></button></h2><button class="icon-button" aria-label={`New workspace on ${machineName(document.on, snapshot.machines, i, snapshot.core)}`} disabled={!ready || treeBusy || !!document.error} title="New workspace" onclick={() => treeAction('new-space', document)}>+</button></header>
        {#each document.spaces as space, j (space.id || j)}
          {@const key = viewKey(document.on, space.id)}
          {@const name = spaceName(space, document.on, snapshot.terminals, `Workspace ${j + 1}`)}
            <div class="tree-node workspace-node">
              <details class="workspace-branch" open={expandedSpaces[key] ?? spaceKey === key}>
                <summary use:longPress={() => spaceMenu(document, space, name)} onclick={event => { event.preventDefault(); expandedSpaces[key] = !(expandedSpaces[key] ?? spaceKey === key); }}><span class="space-badge" aria-hidden="true">{name.slice(0, 1).toUpperCase()}</span><span>{name}</span></summary>
                {#each space.tabs as item, k (item.id || k)}
                  {@const tabName = readable(item.name, String(k + 1))}
                  <div class="tree-node tab-node">
                    <details class="tab-branch" open>
                      <summary use:longPress={() => tabMenu(document, space, item, tabName)}><span aria-hidden="true">▣</span><span>{tabName}</span></summary>
                      <div class="pane-branches">
                        {#each visiblePanes(item.root) as pane, n (pane.id || n)}
                          <button class="pane-link" class:selected={spaceKey === key && tab?.id === item.id && focused === pane.id} aria-current={spaceKey === key && tab?.id === item.id && focused === pane.id ? 'page' : undefined} use:longPress={() => paneMenu(document, space, item, pane, paneTitle(pane, document.on, snapshot.terminals, n))} onclick={() => choosePane(document, space, item, pane)}><span class="pane-number">{n + 1}</span><span>{paneTitle(pane, document.on, snapshot.terminals, n)}</span></button>
                        {:else}<span class="rail-empty">No visible panes</span>{/each}
                      </div>
                    </details>
                    <div class="tree-node-actions">
                      <button class="tree-action tree-create" aria-label={`New pane in ${tabName}`} title="New pane" disabled={!ready || treeBusy || !!document.error} onclick={() => void newPane(document, space, item)}>+</button>
                    </div>
                  </div>
                {:else}<button class="empty-workspace" onclick={() => chooseSpace(key)}>Open empty workspace</button>{/each}
              </details>
              <div class="tree-node-actions">
                <button class="tree-action tree-create" aria-label={`New tab in ${name}`} title="New tab" disabled={!ready || treeBusy || !!document.error} onclick={() => treeAction('new-tab', document, space)}>+</button>
              </div>
            </div>

        {:else}{#if !document.error}<p class="rail-empty">No workspaces yet</p>{/if}{/each}
      </section>
    {:else}<p class="rail-empty">Waiting for computer layouts…</p>{/each}
  </nav>
  {#if treeError}<p class="document-error" role="alert">{treeError}</p>{/if}
  <section class="agent-section" aria-labelledby="drawer-agents-heading">
    <div class="agents-heading"><h2 class="rail-heading" id="drawer-agents-heading">Agents</h2><AgentSortControl mode={agentSortMode} onchange={changeAgentSort} /></div>
    <nav class="agent-navigation" aria-label="Agents and sub-agents">
      {#each agentGroups as group, index (`${group.id}:${index}`)}
        <div class="agent-group-heading">{group.label || 'Workspace'}</div>
        {#each group.members as agent (agent.address)}{@render agentBranch(agent)}{/each}
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
      <ProgramIcon program={terminalProgram(snapshot.terminals.find(terminal => terminalAddress(terminal) === agent.address))} />
      <span class="agent-name">{agent.name}</span>
      <AgentIndicator status={agent.status} />
    </button>
    {#if !target}<span class="agent-unavailable">No visible pane · peek unavailable on web</span>{/if}
  </div>
{/snippet}
