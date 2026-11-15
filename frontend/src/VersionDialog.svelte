<script lang="ts">
  import { onMount } from 'svelte';
  import { channelBase } from './lib/channel';
  import { displayDate, fetchVersionMode, versionRows } from './lib/versions';
  let { close, peerMode = false, serverMode, peerBuild, peerVersion = '', coreVersion, connected = false }: { close: () => void; peerMode?: boolean; serverMode?: unknown; peerBuild?: unknown; peerVersion?: string; coreVersion?: string; connected?: boolean } = $props();
  let dialog: HTMLDialogElement;
  let served = $state((() => serverMode)());
  let status = $state('Refreshing server information…');
  const rows = $derived(versionRows(peerMode, served, peerBuild, peerVersion, coreVersion, connected));
  function dismiss() { dialog.close(); close(); }
  onMount(() => {
    dialog.showModal();
    const controller = new AbortController();
    let active = true;
    const timer = setTimeout(() => controller.abort(), 5000);
    void fetchVersionMode(controller.signal).then(value => {
      if (active) { served = value; status = 'Server information refreshed.'; }
    }).catch(() => {
      if (active) status = 'Refresh failed. Server information is last observed at page load; unavailable fields remain unknown.';
    }).finally(() => clearTimeout(timer));
    return () => { active = false; clearTimeout(timer); controller.abort(); };
  });
</script>

<dialog class="version-dialog" bind:this={dialog} aria-labelledby="version-heading" oncancel={event => { event.preventDefault(); dismiss(); }}>
  <header><h2 id="version-heading">Versions</h2><button class="quiet" aria-label="Close versions" onclick={dismiss}>×</button></header>
  {#if channelBase() === '/main/'}<p class="channel-badge">Main channel</p>{/if}
  <p class="refresh-status" role="status">{status}</p>
  {#each rows as row (row.label)}
    <section aria-label={row.label}>
      <h3>{row.label}</h3><p class="detail">{row.detail}</p>
      {#if !row.unused}
        <dl>
          <dt>Version</dt><dd>{row.build.version || (row.core ? 'Not reported by this Core' : 'Unknown / not reported')}</dd>
          <dt>Commit</dt><dd><code>{row.build.revision || (row.core ? 'Not reported by this Core' : 'Unknown / not reported')}</code></dd>
          <dt>Commit date</dt><dd>{#if row.build.commitDate}<time datetime={row.build.commitDate} title={row.build.commitDate}>{displayDate(row.build.commitDate)}</time>{:else}{row.core ? 'Not reported by this Core' : 'Unknown / not reported'}{/if}</dd>
          {#if !row.core}<dt>Source changes</dt><dd>{row.build.dirty === null ? 'Unknown / not reported' : row.build.dirty ? 'Modified working tree' : 'Clean working tree'}</dd>{/if}
        </dl>
      {/if}
    </section>
  {/each}
  <button class="quiet done" onclick={dismiss}>Close</button>
</dialog>
<style>
  .version-dialog { width: min(38rem, calc(100vw - 24px)); max-height: calc(100dvh - 24px); box-sizing: border-box; padding: 18px; overflow: auto; border: 1px solid var(--muted); border-radius: 12px; background: var(--raised); color: var(--ink); }
  .version-dialog::backdrop { background: #0008; }
  header { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  h2 { font-size: 1.2rem; margin: 0; } h3 { font-size: 1rem; margin: 0 0 4px; }
  button { min-height: 44px; min-width: 44px; } .done { width: 100%; }
  section { border-top: 1px solid var(--muted); padding: 16px 0; }
  p { margin: 4px 0 12px; } .detail, .refresh-status { color: var(--muted); font-size: .85rem; }
  .channel-badge { display: inline-block; font-size: .8rem; padding: 4px 8px; border: 1px solid var(--muted); border-radius: 5px; }
  dl { display: grid; grid-template-columns: 7rem minmax(0, 1fr); gap: 8px; margin: 12px 0 0; font-size: .85rem; }
  dt { color: var(--muted); } dd { margin: 0; overflow-wrap: anywhere; user-select: text; } code { font-size: .8rem; }
  @media (max-width: 380px) { dl { grid-template-columns: 1fr; gap: 4px; } dd { margin-bottom: 6px; } }
</style>
