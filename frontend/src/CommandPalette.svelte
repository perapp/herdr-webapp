<script lang="ts">
  import { onMount, onDestroy, getContext } from 'svelte';
  import { reloadContext } from './lib/mainReload';
  import type { ReloadSafety } from './lib/mainReload';
  const reloadSafety = getContext<ReloadSafety | undefined>(reloadContext);
  const draftKey = {};
  onDestroy(() => reloadSafety?.detach(draftKey));
  import { COMMANDS, COMMAND_SOURCE_LABELS, fetchLiveCommands, mergeCommands, isCommandText, loadFavorites, saveFavorites } from './lib/commands';
  import type { AgentKind, Favorites, LiveCommands } from './lib/commands';
  import { errorMessage } from './lib/api';
  import './command-palette.css';

  let { agent, address, connected, close, insert }: {
    agent: AgentKind;
    address: string;
    connected: boolean;
    close: () => void;
    insert: (event: MouseEvent, text: string, run: boolean) => boolean;
  } = $props();
  const agentNames: Record<AgentKind, string> = { pi: 'Pi', claude: 'Claude', codex: 'Codex', opencode: 'OpenCode' };
  let dialog: HTMLDialogElement;
  let search = $state('');
  let draft = $state('');
  let draftAgent = $state<AgentKind | undefined>();
  let favorites = $state<Favorites>({ pi: [], claude: [], codex: [], opencode: [] });
  let onlyFavorites = $state<Record<AgentKind, boolean>>({ pi: false, claude: false, codex: false, opencode: false });
  let storageMessage = $state('Favorites are saved per agent in this browser.');
  let error = $state('');
  let mounted = $state(false);
  let openedAddress = $state('');
  let openedAgent = $state<AgentKind>();
  let refresh = $state(0);
  let loading = $state(false);
  let live = $state<LiveCommands>();
  let liveAddress = $state('');
  let liveError = $state('');
  let manuallyEdited = $state(false);
  const currentLive = $derived(agent === 'pi' && connected && liveAddress === address ? live : undefined);
  const catalog = $derived(mergeCommands(agent, currentLive?.commands));
  const filtered = $derived(catalog.filter(command =>
    (!onlyFavorites[agent] || favorites[agent].includes(command.id)) &&
    `${command.text} ${command.description}`.toLowerCase().includes(search.trim().toLowerCase())
  ).sort((a, b) => Number(favorites[agent].includes(b.id)) - Number(favorites[agent].includes(a.id))));
  const builtinDraft = $derived(COMMANDS[agent].some(command => draft === command.text || draft.startsWith(`${command.text} `)));
  const valid = $derived(isCommandText(draft) && draftAgent === agent && openedAddress === address && openedAgent === agent &&
    (manuallyEdited || catalog.some(command => command.text === draft)));

  $effect(() => {
    const currentAgent = agent;
    const currentAddress = address;
    const ready = mounted && connected && openedAddress === currentAddress && openedAgent === currentAgent;
    void refresh;
    live = undefined;
    liveAddress = currentAddress;
    liveError = '';
    loading = false;
    if (currentAgent !== 'pi' || !ready) return;
    const controller = new AbortController();
    loading = true;
    fetchLiveCommands(currentAddress, controller.signal).then(result => {
      if (!controller.signal.aborted) live = result;
    }).catch(reason => {
      if (!controller.signal.aborted) liveError = errorMessage(reason).replace(/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/gu, ' ').slice(0, 2048);
    }).finally(() => { if (!controller.signal.aborted) loading = false; });
    return () => controller.abort();
  });

  function dismiss() { reloadSafety?.draft(draftKey, false); dialog.close(); close(); }
  function favorite(id: string) {
    const ids = favorites[agent].includes(id) ? favorites[agent].filter(value => value !== id) : [...favorites[agent], id];
    const result = saveFavorites(agent, ids);
    favorites = result.favorites;
    storageMessage = result.saved ? 'Favorites saved per agent in this browser.' : 'Browser storage is unavailable or invalid. Favorites were not changed; allow storage and reopen Commands to try again.';
  }
  function send(event: MouseEvent, run: boolean) {
    if (!connected) { error = 'Disconnected. Close and reopen Commands after reconnecting.'; return; }
    if (loading && !builtinDraft) { error = 'Wait for live command discovery to finish before inserting or running.'; return; }
    if (!valid) { error = 'Select a command for this agent, then use a single-line slash command with up to 512 argument characters and no control characters.'; return; }
    // Closing the native modal restores terminal focus before the trusted click callback.
    // Keep this synchronous: phone keyboards require the original user gesture.
    dialog.close();
    let accepted = false;
    try { accepted = insert(event, draft, run); } catch { /* Reopen rather than losing the draft. */ }
    if (accepted) { reloadSafety?.draft(draftKey, false); close(); return; }
    dialog.showModal();
    error = 'Nothing was sent. The connection or agent may have changed. Close Commands, check the active terminal and connection, then reopen Commands before trying again.';
  }
  onMount(() => {
    const result = loadFavorites();
    favorites = result.favorites;
    if (!result.saved) storageMessage = 'Browser storage is unavailable or invalid. Favorites cannot be loaded or saved; commands are still available.';
    const viewport = window.visualViewport;
    function fitViewport() {
      const height = viewport?.height ?? window.innerHeight;
      const bottom = Math.max(0, window.innerHeight - (height + (viewport?.offsetTop ?? 0)));
      dialog.style.setProperty('--command-visible-height', `${Math.max(0, height - 16)}px`);
      dialog.style.setProperty('--command-bottom', `${bottom}px`);
      dialog.dataset.compact = String(height < 560);
    }
    fitViewport();
    viewport?.addEventListener('resize', fitViewport);
    viewport?.addEventListener('scroll', fitViewport);
    window.addEventListener('resize', fitViewport);
    dialog.showModal();
    openedAddress = address;
    openedAgent = agent;
    mounted = true;
    return () => {
      viewport?.removeEventListener('resize', fitViewport);
      viewport?.removeEventListener('scroll', fitViewport);
      window.removeEventListener('resize', fitViewport);
    };
  });
</script>

<dialog class="command-palette" bind:this={dialog} aria-labelledby="command-heading"
  oncancel={event => { event.preventDefault(); dismiss(); }}>
  <header class="command-heading">
    <div><p class="command-agent">{agentNames[agent]} / {currentLive?.available ? 'live commands' : 'built-ins'}</p><h2 id="command-heading">Commands</h2></div>
    <button class="quiet" aria-label="Close commands" onclick={dismiss}>×</button>
  </header>
  <div class="command-filters">
    <label for="command-search">Find a command</label>
    <input id="command-search" type="search" placeholder="Search names or descriptions" bind:value={search} />
    <button class="command-favorites-filter" aria-pressed={onlyFavorites[agent]}
      onclick={() => onlyFavorites[agent] = !onlyFavorites[agent]}>★ Favorites only</button>
    <p class="command-note" role="status">{storageMessage}</p>
    {#if agent === 'pi'}
      <div class="command-live">
        <p class="command-note" role="status">{!connected ? 'Connection changed. Close and reopen Commands after reconnecting.' : loading ? 'Loading live Pi commands… Built-ins remain available.' : liveError ? `Live commands unavailable: ${liveError} Reload the Pi command publisher with /reload, then retry.` : currentLive?.message || (currentLive?.available ? 'Live Pi commands loaded.' : 'Live commands unavailable. Reload the Pi command publisher with /reload, then retry.')}</p>
        <button disabled={!connected || loading} onclick={() => refresh++}>{loading ? 'Loading…' : liveError || !currentLive?.available ? 'Retry' : 'Refresh'}</button>
      </div>
    {/if}
  </div>
  <div class="command-list" role="group" aria-label={`${agent} commands`}>
    {#each filtered as command (command.id)}
      <div class="command-row" class:command-chosen={draftAgent === agent && draft === command.text}>
        <button class="command-select" onclick={() => { draft = command.text; reloadSafety?.draft(draftKey, !!draft); draftAgent = agent; manuallyEdited = false; error = ''; }}>
          <code>{command.text}</code><span>{command.description}</span>
          {#if command.source}<small class="command-source">{COMMAND_SOURCE_LABELS[command.source]}</small>{/if}
        </button>
        <button class="command-star" aria-label={`Favorite ${command.text}`} aria-pressed={favorites[agent].includes(command.id)}
          onclick={() => favorite(command.id)}><span aria-hidden="true">{favorites[agent].includes(command.id) ? '★' : '☆'}</span></button>
      </div>
    {:else}
      <p class="command-empty">{onlyFavorites[agent] ? 'No matching favorites. Turn off Favorites only to browse and star commands.' : 'No matching commands. Try another search.'}</p>
    {/each}
  </div>
  <footer class="command-footer">
    <label for="command-draft">Command to insert · editable</label>
    <input id="command-draft" class="command-draft" spellcheck="false" autocapitalize="off" autocomplete="off"
      value={draft} oninput={event => { draft = event.currentTarget.value; reloadSafety?.draft(draftKey, !!draft); draftAgent = agent; manuallyEdited = true; error = ''; }} placeholder="/command arguments" />
    <p class="command-note">Insert adds text only. Run adds text and presses Enter. Your current terminal draft is not cleared; use an empty terminal prompt.</p>
    <p class="command-error" role="alert">{error || (!connected ? 'Disconnected. Reconnect to insert or run.' : draft && !valid ? 'Use a command for this agent: one line, up to 512 argument characters, no terminal controls.' : '')}</p>
    <div class="command-actions"><button onclick={dismiss}>Cancel</button><button disabled={!connected || (loading && !builtinDraft) || !valid} onclick={event => send(event, false)}>Insert</button><button class="primary" disabled={!connected || (loading && !builtinDraft) || !valid} onclick={event => send(event, true)}>Run ↵</button></div>
    <p class="command-provenance">Built-in availability depends on your agent version. {agent === 'pi' ? 'Live commands come from this terminal’s Pi publisher.' : 'Commands use the built-in catalog.'}</p>
  </footer>
</dialog>
