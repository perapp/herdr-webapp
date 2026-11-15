<script lang="ts">
  import SourceLink from './SourceLink.svelte';
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
    <div class="heading-copy"><p class="app-label">Herdr</p><h2 id="settings-heading">Settings</h2></div>
    <button class="dismiss" aria-label="Close settings" onclick={dismiss}>×</button>
  </header>
  <div class="preferences-body" bind:this={body}>
    <section class="settings-section" aria-labelledby="display-heading">
      <div class="section-heading"><h3 id="display-heading">Display</h3><span class="scope-label">This browser</span></div>
      <div class="settings-card">
        <fieldset class="appearance-options">
          <legend>Appearance</legend>
          <div class="appearance-segments">
            {#each choices as choice (choice.value)}
              <label class="appearance-choice" class:chosen={appearance.preference === choice.value}>
                <input type="radio" name="appearance" value={choice.value} checked={appearance.preference === choice.value}
                  onchange={() => saved = setAppearance(choice.value)} />
                <span class="appearance-preview" class:preview-system={choice.value === 'system'} class:preview-light={choice.value === 'light'} class:preview-dark={choice.value === 'dark'} aria-hidden="true">
                  <span class="preview-sidebar"><i></i><i></i><i></i></span>
                  <span class="preview-terminal"><span class="preview-tabs"><i></i><i></i></span><span class="preview-prompt">❯ <i></i></span><span class="preview-line"></span><span class="preview-cursor"></span></span>
                </span>
                <span class="appearance-label">{choice.label}<span class="choice-indicator" aria-hidden="true">{appearance.preference === choice.value ? '✓' : ''}</span></span>
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
        <p class="control-hint unavailable-access">Connect to a computer to manage its access.</p>
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
    <div class="source-row"><SourceLink /></div>
  </footer>
</dialog>
{#if showVersions}<VersionDialog close={closeVersions} {peerMode} {serverMode} {peerBuild} {peerVersion} {coreVersion} {connected} {computerName} />{/if}
{#if installMessage}<InstallDialog message={installMessage} close={closeInstall} />{/if}

<style>
  /* Use the app palette for chrome; only the miniature appearance samples have
     fixed colours, so Light and Dark stay recognisable in either appearance. */
  dialog.preferences { width: min(calc(100% - 32px), 488px); padding: 0; border-radius: 16px; max-height: calc(100dvh - 40px); overscroll-behavior: contain; }
  dialog.preferences[open] { display: flex; flex-direction: column; overflow: hidden; }
  .preferences-header { flex-shrink: 0; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 18px 16px 16px 24px; background: var(--base); border-bottom: 1px solid var(--line); }
  .heading-copy { min-width: 0; }
  .app-label { margin: 0 0 3px; color: var(--muted); font-size: .846154rem; font-weight: 500; letter-spacing: .025em; }
  .preferences-header h2 { margin: 0; font-size: 1.538462rem; line-height: 1.2; font-weight: 650; letter-spacing: -.035em; }
  .dismiss { flex-shrink: 0; width: 44px; min-height: 44px; padding: 0; border: 1px solid transparent; background: transparent; font-size: 1.846154rem; color: var(--muted); border-radius: 50%; }
  .preferences-body { min-height: 0; overflow-y: auto; overscroll-behavior: contain; scrollbar-gutter: stable; scroll-padding-block: 16px; padding: 20px 20px 20px 24px; }
  .settings-section + .settings-section { margin-top: 22px; }
  .section-heading { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; margin: 0 2px 9px; }
  h3 { margin: 0 2px 9px; font-size: .846154rem; font-weight: 650; letter-spacing: .015em; color: var(--muted); }
  .section-heading h3 { margin: 0; }
  .scope-label { color: var(--muted); font-size: .769231rem; }
  .settings-card { background: var(--panel); border: 1px solid var(--line); border-radius: 12px; overflow: hidden; }
  .appearance-options { margin: 0; padding: 16px 16px 0; border: 0; min-width: 0; }
  .appearance-options legend { float: left; width: 100%; padding: 0; margin-bottom: 12px; font-size: 1rem; font-weight: 550; }
  .appearance-segments { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); clear: both; gap: 8px; }
  .appearance-choice { position: relative; display: flex; flex-direction: column; align-items: stretch; gap: 0; min-width: 0; min-height: 44px; margin: 0; padding: 5px; border: 1px solid var(--line); border-radius: 9px; background: var(--base); font-size: .923077rem; font-weight: 500; color: var(--muted); cursor: pointer; }
  .appearance-choice:hover { border-color: var(--muted); }
  .appearance-choice.chosen { background: var(--selected); color: var(--bright); border-color: var(--teal); }
  .appearance-choice:has(input:focus-visible) { outline: 2px solid var(--teal); outline-offset: 3px; }
  .appearance-choice input { position: absolute; width: 1px; height: 1px; min-height: 0; margin: 0; padding: 0; opacity: 0; }
  .appearance-label { display: flex; align-items: center; justify-content: space-between; gap: 3px; padding: 8px 3px 3px; line-height: 1.3; }
  .choice-indicator { display: grid; place-items: center; flex-shrink: 0; width: 14px; height: 14px; border: 1px solid var(--line); border-radius: 50%; font-size: 10px; line-height: 1; }
  .chosen .choice-indicator { color: var(--base); background: var(--teal); border-color: var(--teal); }
  .appearance-preview { --sample-bg: #eff1f5; --sample-panel: #dce0e8; --sample-line: #bcc0cc; --sample-text: #4c4f69; --sample-accent: #1e66f5; display: flex; height: 62px; overflow: hidden; border: 1px solid var(--sample-line); border-radius: 5px; background: var(--sample-bg); }
  .preview-dark { --sample-bg: #1e1e2e; --sample-panel: #313244; --sample-line: #45475a; --sample-text: #cdd6f4; --sample-accent: #89b4fa; }
  .preview-system { --sample-bg: linear-gradient(115deg, #eff1f5 49.5%, #1e1e2e 50.5%); --sample-panel: #dce0e8; --sample-line: #9399b2; --sample-text: #7c84a0; --sample-accent: #7287fd; }
  .preview-sidebar { display: flex; flex-direction: column; gap: 5px; width: 19%; flex-shrink: 0; padding: 9px 4px; background: var(--sample-panel); border-right: 1px solid var(--sample-line); }
  .preview-sidebar i { display: block; height: 3px; border-radius: 1px; background: var(--sample-line); }
  .preview-sidebar i:first-child { background: var(--sample-accent); }
  .preview-terminal { flex: 1; min-width: 0; }
  .preview-tabs { display: flex; gap: 4px; height: 12px; padding: 4px; border-bottom: 1px solid var(--sample-line); }
  .preview-tabs i { width: 25%; border-radius: 1px; background: var(--sample-line); }
  .preview-prompt { display: flex; align-items: center; gap: 4px; margin: 7px 6px 0; color: var(--sample-accent); font: 10px ui-monospace, SFMono-Regular, monospace; }
  .preview-prompt i { width: 45%; height: 3px; background: var(--sample-text); border-radius: 1px; }
  .preview-line { display: block; width: 58%; height: 3px; margin: 4px 6px; background: var(--sample-line); border-radius: 1px; }
  .preview-cursor { display: block; width: 4px; height: 7px; margin: 5px 6px; background: var(--sample-accent); }
  .control-hint { clear: both; margin: 12px 0 16px; color: var(--muted); font-size: .846154rem; line-height: 1.5; }
  .unavailable-access { margin: 0; padding: 16px; border: 1px solid var(--line); border-radius: 12px; background: var(--panel); }
  .terminal-font-options { margin: 0; padding: 16px 16px 10px; border: 0; border-top: 1px solid var(--line); border-radius: 0; min-width: 0; }
  .terminal-font-options legend { float: left; padding: 0; font-size: 1rem; font-weight: 550; }
  .font-caption { display: flex; align-items: baseline; justify-content: space-between; clear: both; gap: 8px; padding: 4px 0 10px; }
  .font-caption p { margin: 0; font-size: .846154rem; color: var(--muted); }
  output { padding: 3px 7px; border: 1px solid var(--line); border-radius: 5px; background: var(--base); font: 500 1rem ui-monospace, SFMono-Regular, monospace; font-variant-numeric: tabular-nums; color: var(--bright); white-space: nowrap; }
  output span { color: var(--muted); font-size: .846154rem; }
  .terminal-font-controls { display: flex; align-items: center; gap: 12px; }
  .terminal-font-controls button { flex-shrink: 0; width: 44px; min-height: 44px; padding: 0; border-radius: 8px; background: var(--base); font-size: 1.384615rem; }
  .terminal-font-controls input[type=range] { min-width: 0; height: 44px; padding: 0; border: 0; background: transparent; accent-color: var(--teal); cursor: pointer; }
  .terminal-font-options .font-size-reset { min-height: 44px; margin: 2px 0 0 -6px; padding: 6px; font-size: .846154rem; color: var(--muted); }
  .app-actions { flex-shrink: 0; display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; padding: 10px 16px; border-top: 1px solid var(--line); background: var(--base); }
  .app-actions button { width: 100%; min-width: 0; min-height: 58px; display: flex; align-items: center; gap: 8px; padding: 8px 10px; border: 1px solid transparent; border-radius: 8px; text-align: left; background: transparent; }
  .app-actions button:hover { border-color: var(--line); }
  .source-row { grid-column: 1 / -1; display: flex; justify-content: center; border-top: 1px solid var(--line); padding-top: 4px; }
  .action-copy { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .action-copy strong { display: block; font-weight: 550; }
  .action-copy small { display: block; margin-top: 3px; color: var(--muted); font-size: .846154rem; line-height: 1.4; }
  .action-arrow { color: var(--muted); font-size: 1.538462rem; }
  .settings-note { margin: 16px 2px 0; color: var(--muted); font-size: .846154rem; line-height: 1.5; }
  @media (max-width: 600px) {
    dialog.preferences { width: 100%; max-width: none; margin: auto auto 0; max-height: calc(100dvh - max(16px, env(safe-area-inset-top))); border-radius: 16px 16px 0 0; padding: 0; }
    .preferences-header { padding: 14px 12px 14px 20px; }
    .preferences-body { padding: 18px 16px 20px; scrollbar-gutter: auto; }
    .app-actions { padding: 8px 12px max(8px, env(safe-area-inset-bottom)); }
  }
  @media (max-width: 360px) {
    .appearance-options { padding-inline: 12px; }
    .appearance-segments { gap: 5px; }
    .appearance-label { padding-inline: 0; font-size: .846154rem; }
  }
</style>
