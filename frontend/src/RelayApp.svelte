<script lang="ts">
  import { channelBase, channelPath } from './lib/channel';
  import { forgetLocalNotifications } from './lib/notifications';
  import { connectionRoute, connectionPath, lastComputer, rememberComputer, forgetSelection, ConnectionGeneration } from './lib/connectionRoute';
  import { forgetView } from './lib/view';
  import { onMount, setContext } from 'svelte';
  import { MainReload, reloadContext, trackReloadActivity } from './lib/mainReload';
  import type { ReloadState } from './lib/mainReload';
  import App from './App.svelte';
  import SettingsDialog from './SettingsDialog.svelte';
  import UpdateProgress from './UpdateProgress.svelte';
  import { displayVersion, modeBuild } from './lib/versions';
  import { parsePairing, listComputers, forgetComputer } from './lib/peerCrypto';
  import type { Computer } from './lib/peerCrypto';
  import { PeerConnection } from './lib/peerConnection';
  import type { PeerTarget, ConnectionState } from './lib/peerConnection';
  import { disconnectedTransport, setApplicationTransport } from './lib/transport';

  let { pairingFragment = null, relayVersion = '', serverMode, sourceRevision = '' }: { pairingFragment?: string | null; relayVersion?: string; serverMode?: unknown; sourceRevision?: string } = $props();
  let update = $state<ReloadState>({ ready: false, critical: false, dirty: false });
  // Bootstrap provenance intentionally stays fixed for this page's lifetime.
  const updater = (() => new MainReload({ mode: serverMode, sourceRevision, main: location.pathname === '/main' || location.pathname.startsWith('/main/'), automatic: false,
    fetch: signal => fetch(channelPath('/api/mode'), { credentials: 'same-origin', cache: 'no-store', signal }),
    storage: { getItem: key => sessionStorage.getItem(key), setItem: (key, value) => sessionStorage.setItem(key, value) },
    reload: () => location.reload(), changed: state => { update = state; }, visible: () => document.visibilityState === 'visible' }))();
  setContext(reloadContext, updater);
  // Enrollment is durable only after authenticated saveComputer succeeds.
  const paired = (() => pairingFragment ? updater.busy() : undefined)();
  let captureActivity = () => {};
  function reloadUpdate() {
    if (update.critical) return;
    if (update.dirty && !confirm('Reload and discard unsent form drafts or selected files? Nothing will be replayed.')) return;
    updater.manual();
  }
  let computers = $state<Computer[]>([]);
  let connectionState = $state<ConnectionState>({ status: 'Your terminals, wherever you are.' });
  let selected = $state('');
  let forceTurn = $state(new URLSearchParams(location.search).get('forceTurn') === '1');
  let connection = $state<PeerConnection>();
  let mounted = true;
  let copied = $state(false);
  let copyError = $state('');
  let loading = $state(true);
  const channel = channelBase() === '/main/' ? 'Main' : 'Stable';
  let showSettings = $state(false);
  const command = `curl -fsSL '${location.origin}${channelPath('/run')}' | sh`;
  const generation = new ConnectionGeneration();
  function stop() {
    generation.invalidate();
    captureActivity();
    connection?.stop(); connection = undefined;
    setApplicationTransport(disconnectedTransport);
    connectionState = { status: 'Your terminals, wherever you are.' };
  }
  async function refresh(current = generation.guard()) {
    const saved = await listComputers();
    if (mounted && current()) computers = saved;
    return saved;
  }
  function connect(target: PeerTarget) {
    const current = generation.guard();
    selected = target.pairing?.machineId || target.computer?.machineId || '';
    rememberComputer(selected);
    connection = new PeerConnection(target, next => {
      if (!mounted || !current()) return;
      // Snapshot before App teardown; removed drafts must not become falsely safe.
      captureActivity();
      if (next.status === 'Pairing expired') paired?.();
      // Relay App is unmounted on loss: no stale terminal, HTTP or upload can replay.
      setApplicationTransport(next.service || disconnectedTransport);
      connectionState = next;
      if (next.service) { paired?.(); void refresh(current).catch(() => {}); }
    }, forceTurn, () => { if (current()) void updater.check(); });
    connection.start();
  }
  function routePath(path: string) { return forceTurn ? `${path}${path.includes('?') ? '&' : '?'}forceTurn=1` : path; }
  function navigate(path: string) {
    history.pushState(null, '', routePath(path));
    void resolveRoute();
  }
  function choose(id: string) { if (id) navigate(connectionPath(id)); }
  function showConnections() { navigate(channelPath('/')); }
  async function resolveRoute(fragment: string | null = null) {
    stop(); selected = ''; loading = true;
    const current = generation.guard();
    try {
      // A pairing capability overrides launch restoration and explicit connection URLs.
      if (fragment) {
        const pairing = await parsePairing(fragment);
        if (!mounted || !current()) return;
        history.replaceState(null, '', routePath(connectionPath(pairing.machineId)));
        connect({ pairing });
        return;
      }
      let route = connectionRoute();
      if (route.kind === 'launch') {
        const remembered = lastComputer();
        history.replaceState(null, '', routePath(remembered ? connectionPath(remembered) : channelPath('/')));
        route = connectionRoute();
      }
      if (route.kind === 'computer') {
        selected = route.id;
        rememberComputer(route.id);
        connectionState = { status: 'Loading computer…' };
      }
      const saved = await refresh(current);
      if (!mounted || !current()) return;
      if (route.kind === 'computer') {
        const computer = saved.find(item => item.machineId === route.id);
        if (computer) connect({ computer });
        else {
          forgetSelection(route.id); selected = '';
          history.replaceState(null, '', routePath(channelPath('/')));
          connectionState = { status: 'This computer is no longer remembered in this browser. Pair it again using a fresh private link.' };
        }
      } else if (route.kind === 'unknown') {
        connectionState = { status: 'Unknown connection destination. Choose a saved computer below.' };
      }
    } catch {
      if (!mounted || !current()) return;
      paired?.();
      connectionState = { status: fragment ? 'Unable to pair' : 'Unable to load computers', error: 'The pairing link may be invalid or expired, or secure browser storage is unavailable. Retry or scan a fresh QR code to pair this computer.' };
    } finally {
      if (mounted && current()) loading = false;
    }
  }
  async function forget() {
    const id = selected;
    if (!id) return;
    // Capture this target before stopping/changing the singleton transport.
    void forgetLocalNotifications().catch(() => {});
    stop(); selected = '';
    const current = generation.guard();
    try {
      await forgetComputer(id);
      forgetView(id); forgetSelection(id); paired?.();
      if (!mounted || !current()) return;
      history.pushState(null, '', routePath(channelPath('/')));
      await refresh(current);
      if (current()) connectionState = { status: 'Computer forgotten on this browser only. Other devices and connector authorization are unchanged.' };
    } catch {
      if (mounted && current()) connectionState = { status: 'Unable to forget this computer', error: 'Browser storage is unavailable. Try again.' };
    }
  }
  function retry() { void resolveRoute(); }
  async function copy() {
    copied = false; copyError = '';
    try { await navigator.clipboard.writeText(command); if (mounted) copied = true; }
    catch { if (mounted) copyError = 'Could not copy. Select and copy the command above.'; }
  }
  onMount(() => {
    mounted = true;
    const activity = trackReloadActivity(document, updater);
    captureActivity = activity.capture;
    const foreground = () => { if (document.visibilityState === 'visible') void updater.check(); };
    document.addEventListener('visibilitychange', foreground);
    void updater.check(); // Also catch old assets racing a newly activated mode response.
    setApplicationTransport(disconnectedTransport);
    const popstate = () => { paired?.(); void resolveRoute(); };
    window.addEventListener('popstate', popstate);
    const fragment = pairingFragment; pairingFragment = null;
    void resolveRoute(fragment);
    return () => { mounted = false; stop(); updater.stop(); activity.stop(); document.removeEventListener('visibilitychange', foreground); window.removeEventListener('popstate', popstate); };
  });
</script>

{#if update.ready}
  <aside class="revision-update" role="status">
    <span>Update ready{update.critical ? ' · Finish pairing, upload or text composition first.' : update.dirty ? ' · Reload discards unsent form drafts and selected files.' : ''}</span>
    <button disabled={update.critical} onclick={reloadUpdate}>Reload</button>
  </aside>
{/if}
{#if connectionState.service}
  {#key selected}
  <App onConnections={showConnections} {serverMode} peerBuild={connectionState.build} peerMode={true} peerVersion={connectionState.version} {relayVersion} peerRoute={connectionState.route} onForget={forget} computers={computers.map(c => ({ id: c.machineId, name: c.name || `Computer ${c.machineId.slice(0, 8)}` }))} selectedComputer={selected} onChooseComputer={choose} />
  {/key}
{:else}
  <main class="login-shell" data-testid="connections-page">
    <section class="login-card" aria-labelledby="relay-heading">
      <details class="connections-menu"><summary aria-label="Open navigation">☰</summary><nav aria-label="App actions">
        <a class="connections-brand" href={channelPath('/')} aria-label="Herdr-webapp — Connections" title="Connections">Herdr-webapp</a>
        <button aria-haspopup="dialog" onclick={() => showSettings = true}>Settings</button>
      </nav></details>
      <header class="start-heading">
        <a class="wordmark" href={channelPath('/')} aria-label="Herdr-webapp — Connections"><span class="herdr-mark" aria-hidden="true">h</span>herdr<span class="web-label">web</span></a>
        <span class="channel-label">{channel}</span>
      </header>
      <h1 id="relay-heading">{selected ? 'Opening your terminals' : computers.length ? 'Your computers' : 'Your terminals, anywhere.'}</h1>
      <p class="start-intro">{selected ? (computers.find(c => c.machineId === selected)?.name || `Computer ${selected.slice(0, 8)}`) : computers.length ? 'Pick up where you left off.' : 'Connect a computer. Bring your spaces with you.'}</p>
      {#if loading || selected || connectionState.error || connectionState.status !== 'Your terminals, wherever you are.'}
        <div class="connection-notice" aria-busy={loading} data-testid="connection-notice">
          <p role="status">{loading && !selected ? 'Loading your computers…' : connectionState.status}</p>
          {#if connectionState.error}<p class="error" role="alert">{connectionState.error}</p>{/if}
          {#if selected || connectionState.error}
            <div class="connection-actions">
              <button onclick={retry}>Retry connection</button>
              {#if selected}<button class="quiet" onclick={showConnections}>Connections</button>{/if}
            </div>
          {/if}
        </div>
      {/if}
      {#if selected}
        <details class="start-details">
          <summary>Connection help</summary>
          <p>Keep the connector running on this computer. If access was revoked, scan a fresh QR code or open its private link to pair again.</p>
          {#if computers.length}
            <label for="paired-computer">Switch computer</label>
            <select id="paired-computer" value={selected} onchange={event => choose(event.currentTarget.value)}>
              <option value="">Choose a computer…</option>
              {#each computers as computer (computer.machineId)}<option value={computer.machineId}>{computer.name || `Computer ${computer.machineId.slice(0, 8)}`}</option>{/each}
            </select>
            <button class="quiet" onclick={forget}>Forget this computer</button>
          {/if}
        </details>
      {:else if computers.length}
        <ul class="connections" aria-label="Remembered computers" data-testid="remembered-computers">
          {#each computers as computer (computer.machineId)}
            <li><a class="computer-link" data-testid="open-computer" href={routePath(connectionPath(computer.machineId))} onclick={event => { if (!event.ctrlKey && !event.metaKey && !event.shiftKey && !event.altKey && event.button === 0) { event.preventDefault(); choose(computer.machineId); } }}>
              <span class="terminal-prompt" aria-hidden="true">&gt;_</span>
              <span class="computer-label"><strong>{computer.name || `Computer ${computer.machineId.slice(0, 8)}`}</strong><span>Open terminal</span></span>
              <span class="open-arrow" aria-hidden="true">→</span>
            </a></li>
          {/each}
        </ul>
      {/if}
      {#snippet setup()}
        <div class="setup-content" data-testid="computer-setup">
          <p>Run this on the computer you want to use. It installs Herdr Core if missing, then installs and starts the {channel.toLowerCase()} connector.</p>
          <div class="command-block">
            <span class="command-caption">On your computer · {channel}</span>
            <pre class="install-command" aria-label={`${channel} setup command`}><code>{command}</code></pre>
            <button class="copy-command" onclick={copy} data-testid="copy-setup-command">{copied ? 'Copied' : 'Copy command'}</button>
          </div>
          <p class="copy-feedback" role="status">{copyError || (copied ? 'Command copied. Run it in your computer’s terminal.' : '')}</p>
          <p class="pair-instruction">Then scan its QR code with your phone camera, or open its private link in this browser.</p>
        </div>
      {/snippet}
      {#if !loading && !selected && !computers.length}
        <section class="first-setup" aria-labelledby="setup-heading">
          <h2 id="setup-heading">Connect your first computer</h2>
          {@render setup()}
        </section>
      {:else}
        <details class="start-details add-computer" data-testid="add-computer">
          <summary>Connect another computer</summary>
          {@render setup()}
        </details>
      {/if}
      <details class="start-details technical-details">
        <summary>Privacy &amp; connection help</summary>
        <p>Pair only computers you trust. The private link grants terminal access; keep it to yourself. Remembered computers are saved in this browser, separately for Stable and Main.</p>
        <p>Pairing pins your computer’s identity. Terminal content, commands and files travel through encrypted WebRTC, directly when possible or through a configured TURN relay.</p>
        <p>Keep the connector running to reconnect. The setup command reuses a compatible Core; it does not replace an incompatible Core automatically. Managed setups can opt out of Core installation with <code>--no-install-core</code>.</p>
        <details class="start-details"><summary>Connection diagnostics</summary><label class="turn-option"><input type="checkbox" bind:checked={forceTurn} disabled={!!connection} /> Force TURN on the next connection</label><p>Relay {displayVersion(modeBuild(serverMode), relayVersion)}. TURN availability depends on this relay’s configuration.</p></details>
      </details>
    </section>
  </main>
{/if}
<UpdateProgress connected={!!connectionState.service} computerId={selected} />
{#if showSettings}<SettingsDialog close={() => showSettings = false} peerMode {serverMode} connected={false} />{/if}
<style>
  .login-shell { padding: 32px 20px; align-items: start; }
  .login-card { position: relative; min-width: 0; width: min(100%, 520px); margin: clamp(0px, 7vh, 72px) 0; padding: 24px; overflow-wrap: anywhere; }
  .start-heading { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; padding-right: 48px; margin-bottom: 28px; }
  .wordmark { text-decoration: none; gap: 7px; font-size: 1.65rem; }
  .channel-label { padding: 3px 6px; border: 1px solid var(--line); border-radius: 4px; color: var(--muted); font-size: .75rem; }
  .login-card h1 { font-size: 1.8rem; line-height: 1.15; letter-spacing: -.035em; margin: 0 0 9px; font-weight: 600; }
  .start-intro { color: var(--muted); margin-bottom: 22px; }
  .connections { list-style: none; margin: 0 0 18px; padding: 0; border-top: 1px solid var(--line); }
  .connections li { border-bottom: 1px solid var(--line); }
  .computer-link { display: flex; align-items: center; gap: 12px; min-height: 76px; padding: 12px 8px; color: var(--bright); text-decoration: none; border-radius: 4px; }
  .computer-link:hover { background: var(--panel); }
  .terminal-prompt { flex-shrink: 0; font: 600 1.15rem ui-monospace, SFMono-Regular, Consolas, monospace; color: var(--teal); }
  .computer-label { display: grid; gap: 4px; min-width: 0; flex: 1; }
  .computer-label strong { font-weight: 600; }
  .computer-label > span { color: var(--teal); font-size: .85rem; }
  .open-arrow { color: var(--teal); flex-shrink: 0; }
  .connection-notice { border-left: 2px solid var(--teal); padding: 10px 14px; background: var(--panel); margin: 18px 0; }
  .connection-notice p { margin: 0 0 8px; }
  .connection-notice p:last-child { margin: 0; }
  .connection-actions { display: flex; flex-wrap: wrap; gap: 6px; }
  .login-card button { min-height: 44px; white-space: normal; overflow-wrap: anywhere; max-width: 100%; }
  .login-card a:focus-visible { outline: 2px solid var(--teal); outline-offset: -2px; }
  .login-card h2 { font-size: 1rem; margin: 0 0 12px; font-weight: 600; }
  .first-setup { border-top: 1px solid var(--line); padding-top: 20px; }
  .setup-content p { font-size: .95rem; margin-bottom: 14px; }
  .command-block { min-width: 0; padding: 12px; border: 1px solid var(--line); border-left: 3px solid var(--teal); border-radius: 5px; background: var(--panel); }
  .command-caption { display: block; font-size: .75rem; color: var(--muted); }
  .copy-command { width: 100%; background: var(--base); }
  .copy-feedback:empty { display: none; }
  .setup-content .copy-feedback { margin: 8px 0 0; font-size: .85rem; }
  .setup-content .pair-instruction { margin: 14px 0 0; }
  .start-details { margin-top: 8px; font-size: .9rem; }
  .start-details > summary { min-height: 44px; padding: 12px 0; cursor: pointer; color: var(--ink); }
  .start-details > p { margin: 4px 0 14px; }
  .add-computer { border-top: 1px solid var(--line); }
  .technical-details { margin-top: 14px; border-top: 1px solid var(--line); color: var(--muted); font-size: .85rem; }
  .turn-option { display: flex; align-items: start; gap: 8px; min-height: 44px; margin-top: 0; font-weight: 400; }
  .turn-option input { flex-shrink: 0; margin-top: 4px; }
  .connections-menu { position: absolute; top: 10px; right: 10px; z-index: 1; margin: 0; }
  .connections-menu summary { min-width: 44px; min-height: 44px; display: grid; place-items: center; cursor: pointer; border-radius: 6px; list-style: none; }
  .connections-menu summary::-webkit-details-marker { display: none; }
  .connections-menu nav { position: absolute; right: 0; width: max(180px, 10rem); max-width: calc(100vw - 44px); padding: 6px; border: 1px solid var(--muted); border-radius: 8px; background: var(--raised); }
  .connections-brand { display: flex; align-items: center; min-height: 44px; padding: 6px 10px; color: var(--bright); font-weight: 650; text-decoration: none; border-bottom: 1px solid var(--line); }
  .connections-brand:hover { color: var(--teal); }
  .connections-brand:focus-visible { outline: 2px solid var(--teal); outline-offset: -2px; }
  .connections-menu button { display: block; text-align: left; width: 100%; min-height: 44px; }
  .revision-update { position: fixed; z-index: 1000; top: env(safe-area-inset-top, 0px); left: 50%; transform: translateX(-50%); width: min(95%, 36rem); display: flex; align-items: center; justify-content: space-between; gap: .6rem; padding: .4rem .7rem; max-height: calc(100dvh - env(safe-area-inset-top, 0px)); overflow: auto; flex-wrap: wrap; overflow-wrap: anywhere; border: 1px solid var(--muted); border-radius: .5rem; background: var(--raised); color: var(--ink); font-size: .8rem; }
  .revision-update button { min-height: 44px; flex-shrink: 0; }
  .install-command { white-space: pre-wrap; overflow-wrap: anywhere; margin: 12px 0; font: .9rem/1.6 ui-monospace, SFMono-Regular, Consolas, monospace; color: var(--bright); }
  select { width: 100%; padding: .7rem; margin: .4rem 0; }
  input[type=checkbox] { display: inline-block; width: auto; min-height: 0; }
  @media (max-width: 480px) {
    .login-shell { padding: max(12px, env(safe-area-inset-top)) 12px max(12px, env(safe-area-inset-bottom)); }
    .login-card { padding: 18px; margin: 0; }
    .start-heading { margin-bottom: 24px; }
    .computer-link { gap: 10px; padding-inline: 4px; }
  }
</style>
