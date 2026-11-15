<script lang="ts">
  import { onMount, tick } from 'svelte';
  import VersionDialog from './VersionDialog.svelte';
  import { initializeAppearance, setAppearance, watchAppearance } from './lib/appearance';
  import type { AppearancePreference } from './lib/appearance';
  import { initializeFontSize, watchFontSize, setFontSize, defaultFontSize, minFontSize, maxFontSize } from './lib/fontSize';
  let { close, peerMode = false, serverMode, peerBuild, peerVersion = '', coreVersion, connected = false }: { close: () => void; peerMode?: boolean; serverMode?: unknown; peerBuild?: unknown; peerVersion?: string; coreVersion?: string; connected?: boolean } = $props();
  let showVersions = $state(false);
  let versionButton: HTMLButtonElement;
  function openVersions() { dialog.close(); showVersions = true; }
  async function closeVersions() { showVersions = false; await tick(); dialog.showModal(); versionButton.focus({ preventScroll: true }); }
  let dialog: HTMLDialogElement;
  let appearance = $state(initializeAppearance());
  let saved = $state(true);
  let fontSaved = $state(true);
  let fontSize = $state(initializeFontSize());
  const choices: { value: AppearancePreference; label: string; detail: string }[] = [
    { value: 'system', label: 'System', detail: 'Follow your device’s appearance' },
    { value: 'light', label: 'Light', detail: 'Light chrome and terminal colours' },
    { value: 'dark', label: 'Dark', detail: 'Dark chrome and terminal colours' },
  ];
  function dismiss() { dialog.close(); close(); }
  onMount(() => {
    const stop = watchAppearance(value => appearance = value);
    const stopFont = watchFontSize(value => fontSize = value);
    dialog.showModal();
    return () => { stop(); stopFont(); };
  });
</script>

<dialog class="settings-dialog" bind:this={dialog} aria-labelledby="settings-heading"
  oncancel={event => { event.preventDefault(); dismiss(); }}>
  <header class="settings-heading">
    <h2 id="settings-heading">Settings</h2>
    <button class="quiet" aria-label="Close settings" onclick={dismiss}>×</button>
  </header>
  <fieldset class="appearance-options">
    <legend>Appearance</legend>
    {#each choices as choice (choice.value)}
      <label class="appearance-choice" class:chosen={appearance.preference === choice.value}>
        <input type="radio" name="appearance" value={choice.value} checked={appearance.preference === choice.value}
          onchange={() => saved = setAppearance(choice.value)} />
        <span><strong>{choice.label}</strong><small>{choice.detail}</small></span>
      </label>
    {/each}
  </fieldset>
  <fieldset class="terminal-font-options">
    <legend>Terminal font size</legend>
    <div class="terminal-font-controls">
      <button aria-label="Decrease terminal font size" disabled={fontSize <= minFontSize} onclick={() => fontSaved = setFontSize(fontSize - 1)}>−</button>
      <input type="range" aria-label="Terminal font size" min={minFontSize} max={maxFontSize} step="1" value={fontSize}
        oninput={event => fontSaved = setFontSize(Number(event.currentTarget.value))} />
      <button aria-label="Increase terminal font size" disabled={fontSize >= maxFontSize} onclick={() => fontSaved = setFontSize(fontSize + 1)}>+</button>
      <output aria-live="polite">{fontSize} px</output>
    </div>
    <button class="quiet font-size-reset" disabled={fontSize === defaultFontSize} onclick={() => fontSaved = setFontSize(defaultFontSize)}>Reset to {defaultFontSize} px</button>
  </fieldset>
  <button class="quiet version-option" bind:this={versionButton} aria-haspopup="dialog" onclick={openVersions}>Version</button>
  <p class="settings-note" role="status">{saved && fontSaved ? 'Applies immediately. Saved only in this browser.' : 'Applied for now. Browser storage is unavailable, so this choice may not survive a reload.'}</p>
</dialog>
{#if showVersions}<VersionDialog close={closeVersions} {peerMode} {serverMode} {peerBuild} {peerVersion} {coreVersion} {connected} />{/if}
<style>.version-option { min-height: 44px; width: 100%; text-align: left; }</style>
