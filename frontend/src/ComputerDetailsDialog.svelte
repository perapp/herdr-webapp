<script lang="ts">
  import { onMount } from 'svelte';
  import { computerIssue } from './lib/computerDetails';
  import { errorMessage } from './lib/api';
  import type { SharedDocument } from './lib/schema';
  let { name, on, document: layout, connected, coreVersion, refresh, close }: {
    name: string; on: string | null; document?: SharedDocument; connected: boolean;
    coreVersion?: string; refresh: () => Promise<void>; close: () => void;
  } = $props();
  let dialog: HTMLDialogElement;
  let busy = $state(false);
  let refreshError = $state('');
  const issue = $derived(computerIssue(layout?.error));
  const refreshIssue = $derived(computerIssue(refreshError));
  const status = $derived(!connected || issue?.disconnected ? 'Disconnected' : issue || !layout ? 'Layout unavailable' : 'Connected');
  const tabs = $derived(layout?.spaces.reduce((count, space) => count + space.tabs.length, 0) ?? 0);
  function dismiss() { dialog.close(); close(); }
  async function reload() {
    if (busy) return;
    busy = true; refreshError = '';
    try { await refresh(); } catch (error) { refreshError = errorMessage(error); }
    finally { busy = false; }
  }
  onMount(() => dialog.showModal());
</script>

<dialog class="computer-dialog" bind:this={dialog} aria-labelledby="computer-details-heading" oncancel={event => { event.preventDefault(); dismiss(); }}>
  <header><div><p class="eyebrow">Computer details</p><h2 id="computer-details-heading">{name}</h2></div><button class="quiet dismiss" aria-label="Close computer details" onclick={dismiss}>×</button></header>
  <div class="details-body">
    <p class="connection-state" role="status"><span class="dot" class:live={status === 'Connected'}></span>{status}</p>
    <dl><dt>Connection</dt><dd>{on === null ? 'Local Herdr Core' : 'Via Herdr Core'}</dd>
      {#if on !== null}<dt>Computer alias</dt><dd>{on}</dd>{/if}
      {#if on === null && coreVersion}<dt>Core version</dt><dd>{coreVersion}</dd>{/if}
      {#if layout && !issue}<dt>Workspaces</dt><dd>{layout.spaces.length}</dd><dt>Tabs</dt><dd>{tabs}</dd>{/if}
    </dl>
    {#if issue}
      <section class="issue" aria-labelledby="computer-issue-heading"><h3 id="computer-issue-heading">{issue.title}</h3><p>{issue.message}</p><p class="recovery">{issue.recovery}</p>{#if issue.hint}<p class="hint">{issue.hint}</p>{/if}
        <details><summary>Technical details</summary><pre>{issue.technical}</pre></details>
      </section>
    {:else if !connected}<section class="issue"><h3>Connection paused</h3><p>The browser is reconnecting to Herdr Core. Status and layout information may be out of date.</p></section>{/if}
    {#if refreshIssue}<p class="refresh-error" role="alert">{refreshIssue.message}</p>{/if}
  </div>
  <footer><button disabled={busy || !connected} onclick={() => void reload()}>{busy ? 'Refreshing…' : 'Refresh status'}</button><button class="quiet" onclick={dismiss}>Close</button></footer>
</dialog>

<style>
  dialog.computer-dialog { width: min(480px, calc(100vw - 24px)); max-height: calc(100dvh - 32px); padding: 0; overflow: hidden; border-radius: 16px; background: var(--base); }
  dialog.computer-dialog[open] { display: flex; flex-direction: column; }
  header { flex-shrink: 0; display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 20px 16px 16px 24px; border-bottom: 1px solid var(--line); }
  .eyebrow { margin-bottom: 5px; }
  h2 { margin: 0; font-size: 1.384615rem; font-weight: 600; letter-spacing: -.025em; overflow-wrap: anywhere; }
  .dismiss { flex-shrink: 0; min-width: 44px; font-size: 1.846154rem; padding: 0; }
  .details-body { min-height: 0; overflow: auto; overscroll-behavior: contain; padding: 20px 24px; }
  .connection-state { display: flex; align-items: center; gap: 8px; color: var(--muted); margin-bottom: 20px; }
  dl { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.5fr); gap: 12px; margin: 0; font-size: .923077rem; }
  dt { color: var(--muted); } dd { margin: 0; overflow-wrap: anywhere; user-select: text; }
  .issue { margin-top: 24px; border: 1px solid var(--line); border-left: 3px solid var(--error); border-radius: 8px; background: var(--panel); padding: 16px; }
  h3 { font-size: 1rem; margin: 0 0 8px; font-weight: 600; }
  p { font-size: .923077rem; line-height: 1.55; overflow-wrap: anywhere; }
  .recovery, .hint { color: var(--muted); }
  .hint { margin-bottom: 0; font-size: .846154rem; }
  details { border-top: 1px solid var(--line); margin-top: 16px; }
  summary { min-height: 44px; padding: 12px 0 0; cursor: pointer; font-size: .846154rem; color: var(--muted); }
  pre { margin: 8px 0 0; white-space: pre-wrap; overflow-wrap: anywhere; font: .846154rem/1.5 ui-monospace, monospace; user-select: text; }
  .refresh-error { color: var(--error); margin: 16px 0 0; }
  footer { flex-shrink: 0; display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 12px 16px max(12px, env(safe-area-inset-bottom)); border-top: 1px solid var(--line); }
  button { min-height: 44px; }
  @media (max-width: 600px) { dialog.computer-dialog { width: 100%; max-width: none; margin: auto auto 0; max-height: calc(100dvh - max(16px, env(safe-area-inset-top))); border-radius: 16px 16px 0 0; } header { padding-left: 20px; } .details-body { padding: 18px 20px; } }
</style>
