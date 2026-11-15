<script lang="ts">
  import { onMount, tick } from 'svelte';
  import VersionDialog from './VersionDialog.svelte';
  import InstallDialog from './InstallDialog.svelte';
  import NotificationSettings from './NotificationSettings.svelte';
  import AccessSettings from './AccessSettings.svelte';
  import { installApp } from './lib/pwa';
  import { initializeAppearance, setAppearance, watchAppearance } from './lib/appearance';
  import type { AppearancePreference } from './lib/appearance';
  import { initializeFontSize, watchFontSize, setFontSize, defaultFontSize, minFontSize, maxFontSize } from './lib/fontSize';
  let { close, peerMode = false, serverMode, peerBuild, peerVersion = '', coreVersion, connected = false, computerName = 'Connected computer' }: { close: () => void; peerMode?: boolean; serverMode?: unknown; peerBuild?: unknown; peerVersion?: string; coreVersion?: string; connected?: boolean; computerName?: string } = $props();
  let showVersions = $state(false);
  let installMessage = $state('');
  let versionButton: HTMLButtonElement;
  let installButton: HTMLButtonElement;
  let dialog: HTMLDialogElement;
  let body: HTMLDivElement;
  let scrollPosition = 0;
  let disposed = false;
  function suspend() { scrollPosition = body.scrollTop; dialog.close(); }
  function openVersions() { suspend(); showVersions = true; }
  async function resume(button: HTMLButtonElement) {
    await tick();
    if (disposed) return;
    dialog.showModal();
    button.focus({ preventScroll: true });
    body.scrollTop = scrollPosition;
  }
  async function closeVersions() { showVersions = false; await resume(versionButton); }
  async function openInstall() {
    suspend();
    // Keep the browser's install prompt inside the original user gesture.
    const result = installApp();
    try { const message = await result; if (!disposed) installMessage = message; }
    catch { if (!disposed) installMessage = 'Installation could not start. Try your browser’s Install app menu, or continue using Herdr here.'; }
  }
  async function closeInstall() { installMessage = ''; await resume(installButton); }
  let appearance = $state(initializeAppearance());
  let saved = $state(true);
  let fontSaved = $state(true);
  let fontSize = $state(initializeFontSize());
  const choices: { value: AppearancePreference; label: string }[] = [
    { value: 'system', label: 'System' },
    { value: 'light', label: 'Light' },
    { value: 'dark', label: 'Dark' },
  ];
  function dismiss() { dialog.close(); close(); }
  onMount(() => {
    const stop = watchAppearance(value => appearance = value);
    const stopFont = watchFontSize(value => fontSize = value);
    dialog.showModal();
    return () => { disposed = true; stop(); stopFont(); };
  });
</script>

<dialog class="settings-dialog preferences" bind:this={dialog} aria-labelledby="settings-heading"
  oncancel={event => { event.preventDefault(); dismiss(); }}>
  <header class="preferences-header">
    <h2 id="settings-heading">Settings</h2>
    <button class="dismiss" aria-label="Close settings" onclick={dismiss}>×</button>
  </header>
  <div class="preferences-body" bind:this={body}>
    <section class="settings-section" aria-labelledby="display-heading">
      <h3 id="display-heading">Display</h3>
      <div class="settings-card">
        <fieldset class="appearance-options">
          <legend>Appearance</legend>
          <div class="appearance-segments">
            {#each choices as choice (choice.value)}
              <label class="appearance-choice" class:chosen={appearance.preference === choice.value}>
                <input type="radio" name="appearance" value={choice.value} checked={appearance.preference === choice.value}
                  onchange={() => saved = setAppearance(choice.value)} />
                <span>{choice.label}</span>
              </label>
            {/each}
          </div>
          <p class="control-hint">{appearance.preference === 'system' ? 'Matches your device’s light or dark appearance.' : 'Applies to the app and terminal colours.'}</p>
        </fieldset>
        <fieldset class="terminal-font-options">
          <legend>Text size</legend>
          <div class="font-caption"><p id="font-scope">Terminal and app text</p><output aria-live="polite">{fontSize}<span> px</span></output></div>
          <div class="terminal-font-controls">
            <button aria-label="Decrease font size" disabled={fontSize <= minFontSize} onclick={() => fontSaved = setFontSize(fontSize - 1)}>−</button>
            <input type="range" aria-label="Font size" aria-describedby="font-scope" aria-valuetext={`${fontSize} pixels`} min={minFontSize} max={maxFontSize} step="1" value={fontSize}
              oninput={event => fontSaved = setFontSize(Number(event.currentTarget.value))} />
            <button aria-label="Increase font size" disabled={fontSize >= maxFontSize} onclick={() => fontSaved = setFontSize(fontSize + 1)}>+</button>
          </div>
          <button class="quiet font-size-reset" disabled={fontSize === defaultFontSize} onclick={() => fontSaved = setFontSize(defaultFontSize)}>Reset to {defaultFontSize} px</button>
        </fieldset>
      </div>
    </section>
    <section class="settings-section" aria-labelledby="notifications-heading">
      <h3 id="notifications-heading">Notifications</h3>
      <NotificationSettings />
    </section>
    <section class="settings-section" aria-labelledby="access-heading">
      <h3 id="access-heading">Access</h3>
      {#if !peerMode || connected}
        <AccessSettings {peerMode} {connected} {computerName} />
      {:else}
        <p class="control-hint">Connect to a computer to manage its access.</p>
      {/if}
    </section>
    <p class="settings-note" role="status">{saved && fontSaved ? 'Display changes are saved in this browser.' : 'Applied for now. Browser storage is unavailable; changes may not survive a reload.'}</p>
  </div>
  <footer class="app-actions" aria-label="App">
    <button class="install-option" bind:this={installButton} aria-haspopup="dialog" onclick={openInstall}>
      <span class="action-copy"><strong>Install app</strong><small>Home screen or desktop</small></span><span class="action-arrow" aria-hidden="true">›</span>
    </button>
    <button class="version-option" bind:this={versionButton} aria-haspopup="dialog" onclick={openVersions}>
      <span class="action-copy"><strong>Version</strong><small>Builds and updates</small></span><span class="action-arrow" aria-hidden="true">›</span>
    </button>
  </footer>
</dialog>
{#if showVersions}<VersionDialog close={closeVersions} {peerMode} {serverMode} {peerBuild} {peerVersion} {coreVersion} {connected} {computerName} />{/if}
{#if installMessage}<InstallDialog message={installMessage} close={closeInstall} />{/if}

<style>
  dialog.preferences { width: min(calc(100% - 32px), 480px); padding: 0; border-radius: 16px; max-height: calc(100dvh - 40px); overscroll-behavior: contain; scroll-padding-top: 76px; }
  dialog.preferences[open] { display: flex; flex-direction: column; overflow: hidden; }
  .preferences-header { flex-shrink: 0; position: sticky; top: 0; z-index: 1; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 12px 16px 12px 24px; background: var(--base); border-bottom: 1px solid var(--line); }
  .preferences-header h2 { margin: 0; font-size: 1.538462rem; font-weight: 650; letter-spacing: -.025em; }
  .dismiss { width: 44px; min-height: 44px; padding: 0; border: 0; background: transparent; font-size: 1.846154rem; color: var(--muted); border-radius: 9px; }
  .preferences-body { min-height: 0; overflow-y: auto; overscroll-behavior: contain; padding: 20px 24px 22px; }
  .settings-section + .settings-section { margin-top: 24px; }
  h3 { margin: 0 0 9px 2px; font-size: .846154rem; font-weight: 650; color: var(--muted); }
  .settings-card { background: var(--panel); border: 1px solid var(--line); border-radius: 12px; overflow: hidden; }
  .appearance-options { margin: 0; padding: 16px 16px 0; border: 0; min-width: 0; }
  .appearance-options legend { float: left; width: 100%; padding: 0; margin-bottom: 10px; font-size: 1rem; font-weight: 550; }
  .appearance-segments { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); clear: both; gap: 3px; padding: 3px; background: var(--base); border: 1px solid var(--line); border-radius: 9px; }
  .appearance-choice { position: relative; display: flex; justify-content: center; gap: 0; min-height: 44px; margin: 0; padding: 10px 4px; border: 1px solid transparent; border-radius: 6px; font-size: .923077rem; font-weight: 500; color: var(--muted); }
  .appearance-choice.chosen { background: var(--selected); color: var(--bright); border-color: var(--line); }
  .appearance-choice:has(input:focus-visible) { outline: 2px solid var(--teal); outline-offset: -2px; }
  .appearance-choice input { position: absolute; width: 1px; height: 1px; min-height: 0; opacity: 0; }
  .control-hint { clear: both; margin: 10px 0 16px; color: var(--muted); font-size: .846154rem; line-height: 1.45; }
  .terminal-font-options { margin: 0; padding: 16px 16px 8px; border: 0; border-top: 1px solid var(--line); border-radius: 0; min-width: 0; }
  .terminal-font-options legend { float: left; padding: 0; font-size: 1rem; font-weight: 550; }
  .font-caption { display: flex; align-items: baseline; justify-content: space-between; clear: both; gap: 8px; padding: 4px 0 10px; }
  .font-caption p { margin: 0; font-size: .846154rem; color: var(--muted); }
  output { font: 500 1rem ui-monospace, SFMono-Regular, monospace; color: var(--bright); white-space: nowrap; }
  output span { color: var(--muted); font-size: .846154rem; }
  .terminal-font-controls { display: flex; align-items: center; gap: 14px; }
  .terminal-font-controls button { border-radius: 8px; background: var(--base); }
  .terminal-font-controls input[type=range] { padding: 0; border: 0; background: transparent; }
  .terminal-font-options .font-size-reset { margin: 4px 0 0 -6px; padding: 6px; font-size: .846154rem; color: var(--muted); }
  .app-actions { flex-shrink: 0; display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; padding: 12px 16px; border-top: 1px solid var(--line); background: var(--base); }
  .app-actions button { width: 100%; min-width: 0; min-height: 64px; display: flex; align-items: center; gap: 8px; padding: 10px 12px; border: 1px solid var(--line); border-radius: 9px; text-align: left; background: var(--panel); }
  .action-copy { flex: 1; min-width: 0; }
  .action-copy strong { display: block; font-weight: 550; }
  .action-copy small { display: block; margin-top: 3px; color: var(--muted); font-size: .846154rem; line-height: 1.4; }
  .action-arrow { color: var(--muted); font-size: 1.538462rem; }
  .settings-note { margin: 16px 2px 0; font-size: .846154rem; line-height: 1.45; }
  @media (max-width: 600px) {
    dialog.preferences { width: 100%; max-width: none; margin: auto auto 0; max-height: calc(100dvh - max(16px, env(safe-area-inset-top))); border-radius: 16px 16px 0 0; padding: 0; }
    .preferences-header { padding-left: 20px; }
    .preferences-body { padding: 18px 16px 20px; }
    .app-actions { padding-bottom: max(12px, env(safe-area-inset-bottom)); }
  }
</style>
