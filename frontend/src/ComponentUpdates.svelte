<script lang="ts">
  import { onMount } from 'svelte';
  import { ApiError, errorMessage, request } from './lib/api';
  import { captureTransportRequest } from './lib/transport';
  import { canUpdateComponent, componentBusy, componentCandidate, parseComponentUpdates, noteUpdate, clearUpdateNotice, updateScope, updateNotice } from './lib/componentUpdates';
  import type { ComponentName, ComponentUpdates, ComponentUpdate } from './lib/componentUpdates';
  let { computerName = 'Connected computer', peerMode = true }: { computerName?: string; peerMode?: boolean } = $props();
  let snapshot = $state<ComponentUpdates | null>(null);
  let pending = $state(false);
  let issue = $state('');
  let unavailable = $state(false);
  let confirmation = $state<{ component: ComponentName; value: ComponentUpdate } | null>(null);
  let disposed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const abort = new AbortController();
  const send = captureTransportRequest();
  const scope = updateScope();
  const keys = $derived<ComponentName[]>(peerMode ? ['connector', 'core'] : ['core']);
  const names: Record<ComponentName, string> = { connector: 'Connector', core: 'Herdr Core' };
  const busy = $derived(pending || !!snapshot && (componentBusy(snapshot.connector) || componentBusy(snapshot.core)));
  async function perform(action?: 'check' | 'install', component?: ComponentName, expectedIdentity?: string) {
    if (pending || disposed) return;
    pending = true; issue = '';
    try {
      const value = await request('/api/component-updates' + (action ? '/' + action : ''), action ? { component, ...(action === 'install' ? { expectedIdentity } : {}) } : undefined, abort.signal, send);
      if (!disposed) { snapshot = parseComponentUpdates(value); unavailable = false; }
    } catch (error) {
      if (!disposed) {
        unavailable = error instanceof ApiError && [404, 405, 501].includes(error.status);
        issue = unavailable ? 'This connector does not support update controls yet. Update it on the computer and reconnect.' : errorMessage(error);
        if (action === 'install' && error instanceof ApiError && error.status >= 400 && error.status < 500) clearUpdateNotice(scope);
      }
    } finally { if (!disposed) pending = false; }
  }
  function ask(component: ComponentName) {
    const value = snapshot?.[component];
    if (!value || busy || !canUpdateComponent(value) || updateNotice(scope)?.state === 'pending') return;
    confirmation = { component, value: { ...value } };
  }
  function install() {
    const captured = confirmation; confirmation = null;
    if (!captured || busy || !canUpdateComponent(captured.value) || snapshot?.[captured.component].identity !== captured.value.identity) return;
    noteUpdate(scope, captured.component, captured.value);
    void perform('install', captured.component, captured.value.identity!);
  }
  onMount(() => {
    async function poll() {
      if (document.visibilityState === 'visible') await perform();
      if (!disposed) timer = setTimeout(poll, snapshot && (componentBusy(snapshot.core) || componentBusy(snapshot.connector)) ? 1500 : 10_000);
    }
    void poll();
    return () => { disposed = true; abort.abort(); clearTimeout(timer); };
  });
</script>

<div class="component-updates" aria-label="Computer updates">
  <p class="scope">Updates on <strong>{computerName}</strong></p>
  {#if issue}<p class="update-error" role="alert">{issue}</p>{/if}
  {#if snapshot}
    {#each keys as key (key)}
      {@const name = key as ComponentName}
      {@const value = snapshot[name]}
      <article aria-label={names[name] + ' updates'} data-component={name}>
        <header><h4>{names[name]}</h4><span class="state">{value.state === 'unchecked' ? 'Not checked' : value.state}</span></header>
        <p class="version">{value.currentVersion || 'Not reported'} <span>· {value.channel || 'Channel not reported'}{name === 'core' ? ' · shared by this computer’s clients' : ''}</span></p>
        {#if value.availableVersion}<p class="candidate">Available: <strong>{componentCandidate(value)}</strong></p>{/if}
        {#if value.instruction}<p class="instruction">{value.instruction}</p>{/if}
        {#if value.error}<p class="update-error" role="alert">{value.error}</p>{/if}
        <div class="actions">
          {#if value.canCheck}<button disabled={busy} onclick={() => void perform('check', name)}>Check for updates</button>{/if}
          {#if canUpdateComponent(value)}<button class="primary" disabled={busy} aria-expanded={confirmation?.component === name} onclick={() => ask(name)}>Update…</button>{/if}
        </div>
        {#if confirmation?.component === name}
          <div class="confirmation" role="group" aria-label={'Confirm ' + names[name] + ' update'}>
            <p>Update {names[name]} to <strong>{componentCandidate(confirmation.value)}</strong> on {computerName}?</p>
            <p class="instruction">{name === 'core' ? 'Core is shared infrastructure. Its supported updater preserves running terminals; the connection may reconnect briefly. No connector or Core update is coupled to this action.' : 'Only this channel’s connector restarts. Herdr Core and terminals stay running. This browser reconnects automatically; disconnected input is not queued.'}</p>
            <div class="actions"><button onclick={() => confirmation = null}>Cancel</button><button class="primary" disabled={busy || snapshot[name].identity !== confirmation.value.identity} onclick={install}>Update {names[name]}</button></div>
          </div>
        {/if}
      </article>
    {/each}
    <p class="policy">Core checks are manual, with compatible updates only. Core never updates automatically.{#if peerMode} The launcher checks its connector channel every 30 seconds; automatic installation requires its explicit opt-in.{/if}</p>
  {:else if !issue}<p role="status">Loading update controls…</p>{/if}
  {#if issue}<button disabled={pending} onclick={() => void perform()}>{unavailable ? 'Retry update controls' : 'Retry'}</button>{/if}
</div>

<style>
  .component-updates { margin-top: 20px; border-top: 1px solid var(--line); padding-top: 16px; }
  .scope { margin: 0 0 12px; font-size: .923077rem; color: var(--muted); overflow-wrap: anywhere; }
  article { margin: 12px 0; padding: 14px; border: 1px solid var(--line); border-radius: 10px; background: var(--panel); min-width: 0; }
  header { display: flex; justify-content: space-between; align-items: baseline; gap: 10px; flex-wrap: wrap; }
  h4 { font-size: 1rem; font-weight: 600; margin: 0; }
  .state { font-size: .846154rem; color: var(--muted); text-transform: capitalize; }
  p { margin: 8px 0; line-height: 1.5; overflow-wrap: anywhere; }
  .version { font-size: .923077rem; } .version span, .instruction, .policy { color: var(--muted); font-size: .846154rem; white-space: pre-wrap; }
  .candidate { font-size: .923077rem; }
  .actions { display: flex; flex-wrap: wrap; gap: 8px; }
  button { min-height: 44px; overflow-wrap: anywhere; }
  .update-error { color: var(--error); font-size: .923077rem; }
  .confirmation { margin-top: 12px; padding-top: 8px; border-top: 1px solid var(--line); }
  .policy { margin-bottom: 0; }
</style>
