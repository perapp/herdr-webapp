<script lang="ts">
  import { transportEvents } from './lib/transport';
  import { onMount, tick } from 'svelte';
  import { errorMessage, request } from './lib/api';
  import { canInstallUpdate, detectUpdatedVersion, parseUpdateStatus, updateBusy } from './lib/updates';
  import type { ReloadState, UpdateStatus } from './lib/updates';
  import './update-panel.css';

  let status = $state<UpdateStatus | null>(null);
  let disconnected = $state(false);
  let issue = $state('');
  let pending = $state(false);
  let pendingInstall = $state(false);
  let confirmation = $state<{ from: string; to: string } | null>(null);
  let dialog = $state<HTMLDialogElement>();
  let disposed = false;
  let revision = 0;
  let generation = 0;
  const abort = new AbortController();
  const reload: ReloadState = { fromVersion: null, reloaded: false };
  const busy = $derived(pending || updateBusy(status));

  function accept(value: unknown) {
    const next = parseUpdateStatus(value);
    status = next;
    issue = '';
    disconnected = false;
    if (detectUpdatedVersion(reload, next)) window.location.reload();
  }
  async function perform(path: string, body?: unknown) {
    const current = ++generation;
    const before = revision;
    pending = true;
    pendingInstall = path === '/api/updates/install';
    issue = '';
    try {
      const value = await request(path, body, abort.signal);
      if (!disposed && current === generation && before === revision) accept(value);
    } catch (error) {
      if (!disposed && current === generation && before === revision) {
        issue = errorMessage(error).slice(0, 2048);
      }
    } finally {
      if (!disposed && current === generation) pending = false;
    }
  }
  async function confirmUpdate() {
    if (!canInstallUpdate(status) || busy || !status?.available_version) return;
    confirmation = { from: status.current_version, to: status.available_version };
    await tick();
    if (!disposed && confirmation) dialog?.showModal();
  }
  // A viewport change can close the navigation drawer. Keep its modal out of
  // that hidden ancestor so the confirmation remains visible and dismissible.
  function portal(node: HTMLDialogElement) {
    document.body.append(node);
    return { destroy() { node.close(); node.remove(); } };
  }
  function dismiss() { dialog?.close(); confirmation = null; }
  function install() {
    const captured = confirmation;
    dismiss(); // Never leave a modal over reconnect; no terminal focus or keyboard gesture.
    if (!captured || busy || !canInstallUpdate(status) || status?.current_version !== captured.from || status.available_version !== captured.to) return;
    reload.fromVersion = captured.from;
    void perform('/api/updates/install', { version: captured.to });
  }
  onMount(() => {
    const source = transportEvents('/api/updates/events');
    source.addEventListener('update', event => {
      if (disposed) return;
      revision++;
      try { accept(JSON.parse((event as MessageEvent).data)); }
      catch (error) { issue = errorMessage(error); }
    });
    source.onerror = () => { if (!disposed) disconnected = true; };
    void perform('/api/updates');
    return () => { disposed = true; generation++; abort.abort(); source.close(); };
  });
</script>

<section class="update-panel" aria-label="Web app updates">
  <div class="update-current">
    <span class="update-version" title="Herdr web app version">Herdr web <strong>{status ? `v${status.current_version}` : '—'}</strong></span>
    <button class="quiet" disabled={busy} onclick={() => void perform('/api/updates/check', {})}>Check updates</button>
  </div>
  {#if status?.available_version && status.available_version !== status.current_version}
    <div class="update-available">
      <span>Version <strong>{status.available_version}</strong> available</span>
      <button disabled={!canInstallUpdate(status) || busy} aria-haspopup="dialog" onclick={confirmUpdate}>Update…</button>
    </div>
    {#if status.source === 'development'}
      <p class="update-note">Source build: install a release first to enable updates. This private repository requires GitHub access on the server.</p>
    {:else if !status.can_install && !updateBusy(status)}
      <p class="update-note">{status.message || 'Install is unavailable. Check the server’s GitHub authentication, then check again.'}</p>
    {/if}
    {#if status.release_url}<a class="update-release" href={status.release_url} target="_blank" rel="noopener noreferrer">Release details ↗</a>{/if}
  {/if}
  <div class="update-feedback" role="status" aria-live="polite">
    {#if disconnected}
      <p>Update status unavailable. Reconnecting automatically…{status?.phase === 'restarting' ? ' The web server is restarting.' : ''}</p>
    {:else if issue}
      <p class="update-error">{issue} Check updates to retry.</p>
    {:else if status?.phase === 'checking' || (pending && status?.phase !== 'downloading' && status?.phase !== 'restarting')}
      <p>{pendingInstall ? 'Starting update…' : 'Checking updates…'}</p>
    {:else if status?.phase === 'downloading' || status?.phase === 'restarting'}
      <p>{status.message || (status.phase === 'downloading' ? 'Downloading update…' : 'Restarting the web server. Reconnecting automatically…')}</p>
    {:else if status?.phase === 'error'}
      <p class="update-error">{status.message || 'Update failed.'} Check updates to retry.</p>
    {:else if status?.message && (!status.available_version || status.can_install)}
      <p>{status.message}</p>
    {/if}
  </div>
</section>

{#if confirmation}
  <dialog class="update-dialog" use:portal bind:this={dialog} aria-labelledby="update-heading" oncancel={event => { event.preventDefault(); dismiss(); }}>
    <h2 id="update-heading">Update Herdr web?</h2>
    <p class="update-transition"><span>v{confirmation.from}</span><span aria-hidden="true">→</span><strong>v{confirmation.to}</strong></p>
    <p>Only the web server restarts. Herdr Core and your terminals keep running. This browser reconnects automatically and loads the new version.</p>
    <p class="update-note">Terminal input is unavailable briefly while reconnecting. Disconnected keystrokes are not queued.</p>
    <div class="update-dialog-actions">
      <button onclick={dismiss}>Cancel</button>
      <button class="primary" disabled={!canInstallUpdate(status) || busy || status?.available_version !== confirmation.to || status.current_version !== confirmation.from} onclick={install}>Update and restart</button>
    </div>
  </dialog>
{/if}
