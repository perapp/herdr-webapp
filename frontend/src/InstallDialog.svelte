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
  <header class="settings-heading"><h2 id="install-heading">Install app</h2><button class="quiet" aria-label="Close install instructions" onclick={dismiss}>×</button></header>
  <p id="install-message">{message}</p>
  <button class="quiet" onclick={dismiss}>Close</button>
</dialog>
<style>.install-dialog { max-width: min(30rem, calc(100vw - 2rem)); } p { line-height: 1.6; } button { min-height: 44px; }</style>
