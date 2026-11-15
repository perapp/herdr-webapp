<script lang="ts">
  import { getContext, onMount } from 'svelte';
  import { channelBase } from './lib/channel';
  import { displayDate, fetchVersionMode, loadedBuild, modeBuild, versionRows } from './lib/versions';
  import { reloadContext } from './lib/mainReload';
  import type { MainReload } from './lib/mainReload';
  import ComponentUpdates from './ComponentUpdates.svelte';
  import UpdatePanel from './UpdatePanel.svelte';
  let { close, peerMode = false, serverMode, peerBuild, peerVersion = '', coreVersion, connected = false, computerName = 'Connected computer' }: { close: () => void; peerMode?: boolean; serverMode?: unknown; peerBuild?: unknown; peerVersion?: string; coreVersion?: string; connected?: boolean; computerName?: string } = $props();
  let dialog: HTMLDialogElement;
  let served = $state((() => serverMode)());
  let status = $state('Refreshing server information…');
  let checking = $state(false);
  let disposed = false;
  let controller: AbortController | undefined;
  const updater = getContext<MainReload | undefined>(reloadContext);
  const loaded = loadedBuild();
  const backend = $derived(modeBuild(served));
  const available = $derived(!!backend.version && !!loaded.version && (backend.revision && loaded.revision ? backend.revision !== loaded.revision : backend.version !== loaded.version));
  const channel = channelBase() === '/main/' ? 'Main' : 'Stable';
  const rows = $derived(versionRows(peerMode, served, peerBuild, peerVersion, coreVersion, connected));
  function dismiss() { dialog.close(); close(); }
  async function checkBrowser() {
    if (checking) return;
    checking = true; status = 'Checking hosted browser app…';
    const abort = new AbortController(); controller = abort;
    const timeout = setTimeout(() => abort.abort(), 5000);
    try {
      const value = await fetchVersionMode(abort.signal);
      if (!disposed) { served = value; status = 'Server information refreshed.'; }
      await updater?.check();
    } catch { if (!disposed) status = 'Check failed. Last observed information is shown. Check again when the server is reachable.'; }
    finally { clearTimeout(timeout); if (!disposed) checking = false; }
  }
  function reloadBrowser() {
    if (updater) {
      if (!confirm('Reload the browser app? Unsent drafts and selected files will be discarded. Keystrokes will not be replayed.')) return;
      updater.manual();
    } else if (confirm('Reload the browser app? Unsent drafts and selected files will be discarded.')) location.reload();
  }
  onMount(() => {
    dialog.showModal(); void checkBrowser();
    return () => { disposed = true; controller?.abort(); };
  });
</script>

<dialog class="version-dialog" bind:this={dialog} aria-labelledby="version-heading" oncancel={event => { event.preventDefault(); dismiss(); }}>
  <header class="versions-header"><h2 id="version-heading">Versions & updates</h2><button class="dismiss quiet" aria-label="Close versions" onclick={dismiss}>×</button></header>
  <div class="versions-body">
    <p class="channel-badge">{channel} channel{connected ? ` · ${computerName}` : ''}</p>
    <section class="browser-updates" aria-label="Browser app updates">
      <h3>Browser app</h3><p class="version-number">{loaded.version || 'Unavailable'}</p>
      <p class="detail">{peerMode ? `Hosted ${channel.toLowerCase()} app · updates only this browser window` : 'Loaded in this browser · webapp host updates are separate'}</p>
      <div class="actions"><button disabled={checking} onclick={() => void checkBrowser()}>{checking ? 'Checking…' : 'Check for updates'}</button>{#if available}<button class="primary" onclick={reloadBrowser}>Update browser app</button>{/if}</div>
      <p class="refresh-status" role="status">{available ? `Browser app ${backend.version} is available. Reload when you are ready.` : status}</p>
      {#if peerMode}<p class="detail">Relay {backend.version || 'Unavailable'} · deployed by the hosting operator, not updated on your computer.</p>{/if}
    </section>
    {#if connected}
      {#if !peerMode}<section class="local-updates" aria-label="Local webapp updates"><h3>Webapp host</h3><p class="detail">Updates the server on {computerName}, not Herdr Core.</p><UpdatePanel /></section>{/if}
      <ComponentUpdates {computerName} {peerMode} />
    {:else}
      <p class="detail">Open a paired computer to check its connector and shared Herdr Core. Computer updates are unavailable while disconnected.</p>
    {/if}
    <details class="build-details">
      <summary>Build versions, provenance & URLs</summary>
      {#each rows as row (row.label)}
        <section aria-label={row.label}>
          <h3>{row.label}</h3><p class="detail">{row.detail}</p>
          {#if !row.unused}
            <dl>
              {#if row.url}<dt>URL</dt><dd><a href={row.url} target="_blank" rel="noopener noreferrer">{row.url}</a></dd>{/if}
              <dt>Version</dt><dd>{row.build.version || (row.core ? 'Not reported by this Core' : 'Unknown / not reported')}</dd>
              <dt>Commit</dt><dd><code>{row.build.revision || (row.core ? 'Not reported by this Core' : 'Unknown / not reported')}</code></dd>
              <dt>Commit date</dt><dd>{#if row.build.commitDate}<time datetime={row.build.commitDate} title={row.build.commitDate}>{displayDate(row.build.commitDate)}</time>{:else}{row.core ? 'Not reported by this Core' : 'Unknown / not reported'}{/if}</dd>
              {#if !row.core}<dt>Source changes</dt><dd>{row.build.dirty === null ? 'Unknown / not reported' : row.build.dirty ? 'Modified working tree' : 'Clean working tree'}</dd>{/if}
            </dl>
          {/if}
        </section>
      {/each}
    </details>
  </div>
  <footer><button class="quiet done" onclick={dismiss}>Close</button></footer>
</dialog>
<style>
  dialog.version-dialog { width: min(540px, calc(100vw - 24px)); max-height: calc(100dvh - 32px); box-sizing: border-box; padding: 0; border-radius: 16px; background: var(--base); color: var(--ink); overflow: hidden; }
  dialog.version-dialog[open] { display: flex; flex-direction: column; }
  .versions-header { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-shrink: 0; padding: 12px 16px 12px 24px; border-bottom: 1px solid var(--line); }
  h2 { font-size: 1.538462rem; margin: 0; min-width: 0; font-weight: 650; letter-spacing: -.025em; overflow-wrap: anywhere; }
  h3 { font-size: 1rem; margin: 0 0 4px; }
  .dismiss { min-width: 44px; font-size: 1.846154rem; padding: 0; }
  .versions-body { min-height: 0; overflow: auto; padding: 20px 24px; overscroll-behavior: contain; }
  button { min-height: 44px; overflow-wrap: anywhere; } .done { width: 100%; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; }
  p { margin: 8px 0 12px; overflow-wrap: anywhere; }
  .version-number { font: 1rem ui-monospace, SFMono-Regular, monospace; color: var(--bright); }
  .detail, .refresh-status { color: var(--muted); font-size: .846154rem; line-height: 1.5; }
  .channel-badge { color: var(--muted); font-size: .846154rem; margin-top: 0; }
  .local-updates { padding-top: 16px; border-top: 1px solid var(--line); }
  .build-details { margin-top: 24px; border-top: 1px solid var(--line); }
  summary { min-height: 44px; padding: 12px 0; cursor: pointer; color: var(--muted); font-size: .923077rem; }
  .build-details section { border-top: 1px solid var(--line); padding: 16px 0; }
  dl { display: grid; grid-template-columns: min(7rem, 35%) minmax(0, 1fr); gap: 8px; margin: 12px 0 0; font-size: .846154rem; }
  dt { color: var(--muted); overflow-wrap: anywhere; } dd { margin: 0; overflow-wrap: anywhere; user-select: text; } code { font-size: .846154rem; }
  footer { flex-shrink: 0; padding: 8px 16px max(12px, env(safe-area-inset-bottom)); border-top: 1px solid var(--line); }
  @media (max-width: 600px) { dialog.version-dialog { width: 100%; max-width: none; margin: auto auto 0; max-height: calc(100dvh - max(16px, env(safe-area-inset-top))); border-radius: 16px 16px 0 0; } .versions-header { padding-left: 20px; } .versions-body { padding: 18px 16px; } }
  @media (max-width: 380px) { dl { grid-template-columns: 1fr; gap: 4px; } dd { margin-bottom: 6px; } }
</style>
