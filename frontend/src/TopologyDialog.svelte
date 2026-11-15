<script lang="ts">
  import { onMount } from 'svelte';
  import { errorMessage, request } from './lib/api';
  import type { SharedDocument } from './lib/schema';
  export type Action = 'rename-space' | 'rename-tab' | 'new-space' | 'new-tab' | 'split' | 'close-space' | 'close-tab' | 'close-pane';
  let { action, document, space, tab, pane, initialName = '', impact, machine, close, saved }: {
    action: Action; document: SharedDocument; space?: string; tab?: string; pane?: string;
    initialName?: string; impact?: { closes: 'pane' | 'tab' | 'space'; terminals: number }; machine: string; close: () => void; saved: (result: unknown) => Promise<void>;
  } = $props();
  let dialog: HTMLDialogElement;
  let name = $state('');
  let folder = $state('');
  let direction = $state<'right' | 'down'>('right');
  let busy = $state(false);
  let error = $state('');
  const titles: Record<Action, string> = { 'rename-space': 'Rename space', 'rename-tab': 'Rename tab', 'new-space': 'New space', 'new-tab': 'New tab', split: 'Split pane', 'close-space': 'Close workspace', 'close-tab': 'Close tab', 'close-pane': 'Close pane' };
  const closing = $derived(action.startsWith('close-'));
  // The server applies the same cascade; say so before anything ends.
  const cascade = $derived(!impact ? '' : action === 'close-pane' && impact.closes === 'tab' ? 'This is the tab’s last pane, so the tab closes too.'
    : action !== 'close-space' && impact.closes === 'space' ? 'This is the workspace’s last tab, so the workspace closes too.' : '');
  const ending = $derived(!impact ? '' : impact.terminals ? `This ends ${impact.terminals} terminal${impact.terminals === 1 ? '' : 's'} and any sub-agents ${impact.terminals === 1 ? 'it' : 'they'} started. Running programs are stopped and cannot be restored.` : 'No terminals are running in it.');
  onMount(() => { name = initialName; dialog.showModal(); });
  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (busy) return;
    busy = true; error = '';
    try {
      const result = await request('/api/topology', {
        action, on: document.on, version: document.version, space, tab, pane,
        ...(closing ? {} : action === 'split' ? { direction } : { name: name.trim() }),
        ...(action === 'new-space' ? { folder: folder.trim() } : {}),
      });
      await saved(result); close();
    } catch (e) { error = `${errorMessage(e)} Refresh the layout before retrying if it changed elsewhere.`; }
    finally { busy = false; }
  }
</script>

<dialog bind:this={dialog} oncancel={(event) => { if (busy) event.preventDefault(); else close(); }} aria-labelledby="dialog-title">
  <form onsubmit={submit}>
    <p class="eyebrow">Shared layout · {machine}</p>
    <h2 id="dialog-title">{titles[action]}</h2>
    <p class="muted">This changes the layout on the machine, not just this browser.</p>
    {#if closing}
      <p>{ending}</p>
      {#if cascade}<p>{cascade}</p>{/if}
    {:else if action === 'split'}
      <label for="direction">Direction</label>
      <select id="direction" bind:value={direction}><option value="right">Side by side</option><option value="down">Above and below</option></select>
    {:else}
      <label for="layout-name">Name</label>
      <input id="layout-name" bind:value={name} required maxlength="160" autocomplete="off" placeholder={action.includes('space') ? 'Workspace name' : 'Tab name'} />
    {/if}
    {#if action === 'new-space'}
      <label for="folder">Working folder <span class="muted">(optional)</span></label>
      <input id="folder" bind:value={folder} maxlength="4096" autocomplete="off" placeholder="Machine default" />
    {/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="dialog-actions"><button type="button" disabled={busy} onclick={close}>Cancel</button><button class="primary" class:danger={closing} type="submit" disabled={busy}>{busy ? (closing ? 'Closing…' : 'Saving…') : titles[action]}</button></div>
  </form>
</dialog>

<style>
  .primary.danger { background: var(--error); border-color: var(--error); }
</style>
