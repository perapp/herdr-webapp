<script lang="ts">
  import { onMount } from 'svelte';
  let { message, close }: { message: string; close: () => void } = $props();
  let dialog: HTMLDialogElement;
  function dismiss() { dialog.close(); close(); }
  onMount(() => { dialog.showModal(); });
</script>
<dialog class="settings-dialog install-dialog" bind:this={dialog} aria-labelledby="install-heading" aria-describedby="install-message"
  oncancel={event => { event.preventDefault(); dismiss(); }}
  onclick={event => { if (event.target === dialog) dismiss(); }}>
  <header class="install-header"><h2 id="install-heading">Install app</h2><button class="dismiss" aria-label="Close install instructions" onclick={dismiss}>×</button></header>
  <div class="install-body">
    <p class="install-intro">Herdr, a tap away.</p>
    <p id="install-message">{message}</p>
  </div>
  <footer><button onclick={dismiss}>Done</button></footer>
</dialog>
<style>
  dialog.install-dialog { width: min(420px, calc(100% - 32px)); padding: 0; border-radius: 16px; overflow: hidden; max-height: calc(100dvh - 40px); }
  dialog.install-dialog[open] { display: flex; flex-direction: column; }
  .install-header { display: flex; flex-shrink: 0; justify-content: space-between; align-items: center; gap: 12px; padding: 12px 16px 12px 24px; border-bottom: 1px solid var(--line); }
  h2 { margin: 0; font-size: 1.538462rem; font-weight: 650; letter-spacing: -.025em; }
  .dismiss { width: 44px; padding: 0; border: 0; background: transparent; color: var(--muted); font-size: 1.846154rem; }
  button { min-height: 44px; border-radius: 9px; }
  .install-body { min-height: 0; overflow-y: auto; overscroll-behavior: contain; padding: 20px 24px; }
  p { margin: 0; line-height: 1.6; overflow-wrap: anywhere; }
  .install-intro { margin-bottom: 8px; font-weight: 550; color: var(--bright); }
  #install-message { color: var(--muted); }
  footer { flex-shrink: 0; padding: 12px 24px max(16px, env(safe-area-inset-bottom)); border-top: 1px solid var(--line); }
  footer button { width: 100%; background: var(--panel); }
  @media (max-width: 600px) {
    dialog.install-dialog { width: 100%; max-width: none; margin: auto auto 0; max-height: calc(100dvh - max(16px, env(safe-area-inset-top))); border-radius: 16px 16px 0 0; padding: 0; }
    .install-header { padding-left: 20px; }
    .install-body { padding: 20px; }
    footer { padding-left: 20px; padding-right: 20px; }
  }
</style>
