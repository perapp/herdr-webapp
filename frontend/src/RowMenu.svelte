<script lang="ts">
  import { onMount } from 'svelte';
  export interface RowMenuItem { label: string; danger?: boolean; disabled?: boolean; run: () => void }
  let { title, items, close }: { title: string; items: RowMenuItem[]; close: () => void } = $props();
  let dialog: HTMLDialogElement;
  // Modal, so it stacks above the navigation drawer it was opened from.
  onMount(() => dialog.showModal());
</script>

<dialog class="row-menu" bind:this={dialog} aria-label={`Actions for ${title}`} onclose={close} onpointerdown={event => { if (event.target === dialog) dialog.close(); }}>
  <p class="eyebrow">{title}</p>
  <div class="row-menu-items">
    {#each items as item (item.label)}
      <button class:danger={item.danger} disabled={item.disabled} onclick={() => { dialog.close(); item.run(); }}>{item.label}</button>
    {/each}
  </div>
  <button class="row-menu-cancel" onclick={() => dialog.close()}>Cancel</button>
</dialog>

<style>
  dialog.row-menu { width: min(calc(100% - 32px), 320px); padding: 14px; }
  .eyebrow { margin: 4px 8px 8px; overflow-wrap: anywhere; }
  .row-menu-items { display: flex; flex-direction: column; }
  .row-menu-items button, .row-menu-cancel { width: 100%; min-height: 44px; padding: 6px 8px; border: 0; background: transparent; text-align: left; }
  .row-menu-items button:not(:disabled):hover, .row-menu-cancel:hover { background: var(--raised); }
  .row-menu-items button.danger:not(:disabled) { color: var(--error); }
  .row-menu-cancel { margin-top: 6px; border-top: 1px solid var(--line); color: var(--muted); }
  @media (max-width: 600px) {
    dialog.row-menu { margin: auto auto 0; width: 100%; max-width: none; border-radius: 14px 14px 0 0; padding: 12px 12px max(12px, env(safe-area-inset-bottom)); }
  }
</style>
