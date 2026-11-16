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
  import { initializeTerminalPrefs, setTerminalPref, watchGrid, watchTerminalPrefs } from './lib/terminalPrefs';
  import type { TerminalGrid, TerminalPrefs } from './lib/terminalPrefs';
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
  let prefs = $state(initializeTerminalPrefs());
  let prefsSaved = $state(true);
  let grid = $state<TerminalGrid>();
  // The two keyboard switches only mean something with an on-screen keyboard.
  const touch = typeof matchMedia === 'function' && matchMedia('(pointer: coarse)').matches;
  const switches = ([
    { name: 'autoKeys', title: 'Keys with the keyboard', detail: 'Show Esc, Tab, Ctrl, arrows and the attach row whenever the on-screen keyboard opens.' },
    { name: 'holdGrid', title: 'Steady size under the keyboard', detail: 'Keep the terminal’s size when the keyboard opens, so programs do not redraw or repeat their output.' },
    { name: 'gpu', title: 'Smooth drawing', detail: 'Draw text on the GPU for fluid scrolling. Turn off if text looks wrong on this device.' },
  ] satisfies { name: keyof TerminalPrefs; title: string; detail: string }[]).filter(item => touch || item.name === 'gpu');
  const choices: { value: AppearancePreference; label: string }[] = [
    { value: 'system', label: 'Auto' },
    { value: 'light', label: 'Light' },
    { value: 'dark', label: 'Dark' },
  ];
  function dismiss() { dialog.close(); close(); }
  onMount(() => {
    const stop = watchAppearance(value => appearance = value);
    const stopFont = watchFontSize(value => fontSize = value);
    const stopPrefs = watchTerminalPrefs(value => prefs = value);
    const stopGrid = watchGrid(value => grid = value);
    dialog.showModal();
    return () => { disposed = true; stop(); stopFont(); stopPrefs(); stopGrid(); };
  });
</script>

<dialog class="settings-dialog preferences" bind:this={dialog} aria-labelledby="settings-heading"
  oncancel={event => { event.preventDefault(); dismiss(); }}>
  <header class="preferences-header">
    <h2 id="settings-heading">Settings</h2>
    <button class="dismiss" aria-label="Close settings" onclick={dismiss}>Done</button>
  </header>
  <div class="preferences-body" bind:this={body}>
    <section class="settings-section" aria-labelledby="display-heading">
      <h3 id="display-heading">Display</h3>
      <div class="group">
        <fieldset class="text-size">
          <legend>Text size</legend>
          <div class="sample" aria-hidden="true" style:font-size={`${fontSize}px`}>
            <div><b>❯</b> git status</div>
            <div>On branch <i>main</i>, nothing to commit</div>
            <div><b>❯</b> <span class="sample-cursor"></span></div>
          </div>
          <div class="size-row">
            <button aria-label="Decrease font size" disabled={fontSize <= minFontSize} onclick={() => fontSaved = setFontSize(fontSize - 1)}>A</button>
            <input type="range" aria-label="Font size" aria-valuetext={`${fontSize} pixels`} min={minFontSize} max={maxFontSize} step="1" value={fontSize}
              oninput={event => fontSaved = setFontSize(Number(event.currentTarget.value))} />
            <button class="larger" aria-label="Increase font size" disabled={fontSize >= maxFontSize} onclick={() => fontSaved = setFontSize(fontSize + 1)}>A</button>
          </div>
          <div class="size-readout">
            <output aria-live="polite"><strong>{fontSize} px</strong>{#if grid}<span>{` · ${grid.cols} columns × ${grid.rows} rows`}</span>{/if}</output>
            <button class="quiet" disabled={fontSize === defaultFontSize} onclick={() => fontSaved = setFontSize(defaultFontSize)}>Reset</button>
          </div>
          {#if grid && grid.cols < 60}<p class="hint">Programs that draw tables and diffs read best at 60 columns or more. Smaller text or landscape gives more.</p>{/if}
        </fieldset>
        <fieldset class="appearance-options">
          <legend>Appearance</legend>
          <div class="segments">
            {#each choices as choice (choice.value)}
              <label class="segment" class:chosen={appearance.preference === choice.value}>
                <input type="radio" name="appearance" value={choice.value} checked={appearance.preference === choice.value}
                  onchange={() => saved = setAppearance(choice.value)} />
                <span class="swatch" class:swatch-system={choice.value === 'system'} class:swatch-light={choice.value === 'light'} class:swatch-dark={choice.value === 'dark'} aria-hidden="true"></span>
                <span>{choice.label}</span>
              </label>
            {/each}
          </div>
          {#if appearance.preference === 'system'}<p class="hint">Follows this device’s light or dark setting.</p>{/if}
        </fieldset>
      </div>
    </section>
    <section class="settings-section" aria-labelledby="terminal-heading">
      <h3 id="terminal-heading">Terminal</h3>
      <div class="group">
        {#each switches as item (item.name)}
          <label class="switch-row">
            <span class="switch-copy"><strong>{item.title}</strong><small>{item.detail}</small></span>
            <input type="checkbox" role="switch" checked={prefs[item.name]} onchange={event => prefsSaved = setTerminalPref(item.name, event.currentTarget.checked)} />
            <span class="switch" aria-hidden="true"></span>
          </label>
        {/each}
      </div>
    </section>
    <section class="settings-section" aria-labelledby="access-heading">
      <h3 id="access-heading">Access</h3>
      {#if !peerMode || connected}
        <AccessSettings {peerMode} {connected} {computerName} />
      {:else}
        <p class="hint unavailable-access">Connect to a computer to manage its access.</p>
      {/if}
    </section>
    <section class="settings-section" aria-labelledby="notifications-heading">
      <h3 id="notifications-heading">Notifications</h3>
      <NotificationSettings />
    </section>
    <section class="settings-section" aria-labelledby="about-heading">
      <h3 id="about-heading">About</h3>
      <div class="group app-actions">
        <button class="install-option" bind:this={installButton} aria-haspopup="dialog" onclick={openInstall}>
          <span class="action-copy"><strong>Install app</strong><small>Home screen or desktop</small></span><span class="action-arrow" aria-hidden="true">›</span>
        </button>
        <button class="version-option" bind:this={versionButton} aria-haspopup="dialog" onclick={openVersions}>
          <span class="action-copy"><strong>Version</strong><small>Builds and updates</small></span><span class="action-arrow" aria-hidden="true">›</span>
        </button>
      </div>
      <div class="source-row"><SourceLink /></div>
    </section>
    <p class="settings-note" role="status">{saved && fontSaved && prefsSaved ? 'Display and terminal settings are saved in this browser only.' : 'Applied for now. Browser storage is unavailable; changes may not survive a reload.'}</p>
  </div>
</dialog>
{#if showVersions}<VersionDialog close={closeVersions} {peerMode} {serverMode} {peerBuild} {peerVersion} {coreVersion} {connected} {computerName} />{/if}
{#if installMessage}<InstallDialog message={installMessage} close={closeInstall} />{/if}

<style>
  /* A grouped list, as phone settings are: section title, then one rounded
     group of rows. App palette throughout; only the appearance swatches and
     nothing else use fixed colours, so Light and Dark stay recognisable. */
  dialog.preferences { width: min(calc(100% - 32px), 520px); padding: 0; border-radius: 16px; max-height: calc(100dvh - 40px); overscroll-behavior: contain; background: var(--base); }
  dialog.preferences[open] { display: flex; flex-direction: column; overflow: hidden; }
  .preferences-header { flex-shrink: 0; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 10px 10px 20px; border-bottom: 1px solid var(--line); }
  .preferences-header h2 { margin: 0; font-size: 1.307692rem; line-height: 1.2; font-weight: 650; letter-spacing: -.02em; }
  .dismiss { flex-shrink: 0; min-width: 64px; min-height: 44px; padding: 0 12px; border: 0; background: transparent; color: var(--teal); font-weight: 600; border-radius: 8px; }
  .dismiss:hover { background: var(--panel); }
  .preferences-body { min-height: 0; overflow-y: auto; overscroll-behavior: contain; scroll-padding-block: 16px; padding: 4px 16px 20px; }
  .settings-section { margin-top: 20px; }
  h3 { margin: 0 4px 8px; font-size: .846154rem; font-weight: 650; letter-spacing: .06em; text-transform: uppercase; color: var(--muted); }
  .group { background: var(--panel); border: 1px solid var(--line); border-radius: 12px; overflow: hidden; }
  .group > * + * { border-top: 1px solid var(--line); }
  fieldset { margin: 0; padding: 14px 16px 16px; border: 0; border-radius: 0; min-width: 0; }
  legend { float: left; width: 100%; padding: 0; margin-bottom: 10px; font-size: 1rem; font-weight: 550; }
  legend + * { clear: both; }
  .hint { margin: 10px 0 0; color: var(--muted); font-size: .846154rem; line-height: 1.5; }

  /* Text size: what it will look like, then the control, then what it means. */
  .sample { height: 84px; padding: 10px 12px; overflow: hidden; border: 1px solid var(--line); border-radius: 8px; background: var(--base); color: var(--ink); font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; line-height: 1.35; white-space: nowrap; }
  .sample b { color: var(--teal); font-weight: 600; }
  .sample i { color: var(--bright); font-style: normal; }
  .sample-cursor { display: inline-block; width: .6em; height: 1.1em; vertical-align: text-bottom; background: var(--ink); opacity: .7; }
  .size-row { display: flex; align-items: center; gap: 8px; margin-top: 10px; }
  .size-row button { flex-shrink: 0; width: 44px; min-height: 44px; padding: 0; border-radius: 8px; background: var(--base); font-size: .846154rem; font-weight: 600; }
  .size-row button.larger { font-size: 1.461538rem; }
  .size-row input[type=range] { flex: 1; min-width: 0; height: 44px; margin: 0; padding: 0; border: 0; background: transparent; accent-color: var(--teal); cursor: pointer; }
  .size-readout { display: flex; align-items: center; justify-content: space-between; gap: 8px; min-height: 44px; }
  output { min-width: 0; font-variant-numeric: tabular-nums; color: var(--muted); font-size: .923077rem; overflow-wrap: anywhere; }
  output strong { color: var(--bright); font-weight: 600; }
  output span { white-space: pre-wrap; }
  .size-readout .quiet { flex-shrink: 0; min-height: 44px; padding: 0 10px; border: 0; background: transparent; color: var(--teal); font-size: .923077rem; }
  .size-readout .quiet:disabled { color: var(--muted); opacity: .5; }
  .size-readout + .hint { margin-top: 2px; }

  /* Appearance: one segmented control. */
  .segments { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 2px; padding: 3px; border: 1px solid var(--line); border-radius: 10px; background: var(--base); }
  .segment { position: relative; display: flex; align-items: center; justify-content: center; gap: 7px; min-width: 0; min-height: 44px; margin: 0; padding: 4px 6px; border-radius: 7px; color: var(--muted); font-size: .923077rem; cursor: pointer; }
  .segment input { position: absolute; inset: 0; width: 100%; height: 100%; margin: 0; opacity: 0; cursor: pointer; }
  .segment.chosen { background: var(--selected); color: var(--bright); font-weight: 600; box-shadow: 0 0 0 1px var(--line); }
  .segment:has(input:focus-visible) { outline: 2px solid var(--teal); outline-offset: 1px; }
  .swatch { flex-shrink: 0; width: 14px; height: 14px; border-radius: 50%; border: 1px solid #9399b2; background: #eff1f5; }
  .swatch-dark { background: #1e1e2e; }
  .swatch-system { background: linear-gradient(115deg, #eff1f5 49%, #1e1e2e 51%); }

  /* Switch rows. The real checkbox covers the row so the whole row toggles. */
  .switch-row { position: relative; display: flex; align-items: center; gap: 14px; min-height: 60px; margin: 0; padding: 12px 16px; cursor: pointer; }
  .switch-copy { flex: 1; min-width: 0; }
  .switch-copy strong { display: block; font-weight: 550; }
  .switch-copy small { display: block; margin-top: 3px; color: var(--muted); font-size: .846154rem; line-height: 1.45; }
  .switch-row input { position: absolute; inset: 0; width: 100%; height: 100%; margin: 0; opacity: 0; cursor: pointer; }
  .switch { flex-shrink: 0; position: relative; width: 46px; height: 28px; border-radius: 14px; border: 1px solid var(--line); background: var(--base); transition: background .15s, border-color .15s; }
  .switch::after { content: ""; position: absolute; top: 2px; left: 2px; width: 22px; height: 22px; border-radius: 50%; background: var(--muted); transition: transform .15s, background .15s; }
  .switch-row input:checked + .switch { background: var(--teal); border-color: var(--teal); }
  .switch-row input:checked + .switch::after { transform: translateX(18px); background: var(--base); }
  .switch-row:has(input:focus-visible) { outline: 2px solid var(--teal); outline-offset: -2px; }
  @media (prefers-reduced-motion: reduce) { .switch, .switch::after { transition: none; } }

  .unavailable-access { margin: 0; padding: 16px; border: 1px solid var(--line); border-radius: 12px; background: var(--panel); }
  .app-actions button { width: 100%; min-width: 0; min-height: 60px; display: flex; align-items: center; gap: 8px; padding: 10px 12px 10px 16px; border: 0; border-radius: 0; text-align: left; background: transparent; }
  .app-actions button + button { border-top: 1px solid var(--line); }
  .app-actions button:hover { background: var(--raised); }
  .action-copy { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .action-copy strong { display: block; font-weight: 550; }
  .action-copy small { display: block; margin-top: 3px; color: var(--muted); font-size: .846154rem; line-height: 1.4; }
  .action-arrow { color: var(--muted); font-size: 1.538462rem; }
  .source-row { display: flex; justify-content: center; margin-top: 8px; }
  .settings-note { margin: 14px 4px 0; color: var(--muted); font-size: .846154rem; line-height: 1.5; text-align: center; }
  @media (max-width: 600px) {
    /* A full sheet on phones, as a native settings screen is. */
    dialog.preferences { width: 100%; max-width: none; height: 100dvh; max-height: 100dvh; margin: 0; border: 0; border-radius: 0; padding: 0; }
    .preferences-header { padding: max(8px, env(safe-area-inset-top)) 8px 8px 16px; }
    .preferences-body { padding: 0 12px max(20px, env(safe-area-inset-bottom)); }
  }
</style>
