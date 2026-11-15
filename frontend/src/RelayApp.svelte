<script lang="ts">
  import { channelPath } from './lib/channel';
  import { forgetLocalNotifications } from './lib/notifications';
  import { connectionRoute, connectionPath, lastComputer, rememberComputer, forgetSelection, ConnectionGeneration } from './lib/connectionRoute';
  import { forgetView } from './lib/view';
  import { onMount, setContext } from 'svelte';
  import { MainReload, reloadContext, trackReloadActivity } from './lib/mainReload';
  import type { ReloadState } from './lib/mainReload';
  import App from './App.svelte';
  import SettingsDialog from './SettingsDialog.svelte';
  import InstallDialog from './InstallDialog.svelte';
  import { installApp } from './lib/pwa';
  import { parsePairing, listComputers, forgetComputer } from './lib/peerCrypto';
  import type { Computer } from './lib/peerCrypto';
  import { PeerConnection } from './lib/peerConnection';
  import type { PeerTarget, ConnectionState } from './lib/peerConnection';
  import { disconnectedTransport, setApplicationTransport } from './lib/transport';

  let { pairingFragment = null, relayVersion = '', serverMode, sourceRevision = '' }: { pairingFragment?: string | null; relayVersion?: string; serverMode?: unknown; sourceRevision?: string } = $props();
  let update = $state<ReloadState>({ ready: false, critical: false, dirty: false });
  // Bootstrap provenance intentionally stays fixed for this page's lifetime.
  const updater = (() => new MainReload({ mode: serverMode, sourceRevision, main: location.pathname === '/main' || location.pathname.startsWith('/main/'),
    fetch: signal => fetch(channelPath('/api/mode'), { credentials: 'same-origin', cache: 'no-store', signal }),
    storage: { getItem: key => sessionStorage.getItem(key), setItem: (key, value) => sessionStorage.setItem(key, value) },
    reload: () => location.reload(), changed: state => { update = state; }, visible: () => document.visibilityState === 'visible' }))();
  setContext(reloadContext, updater);
  // The one-use capability is only durable after authenticated saveComputer succeeds.
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
  let showSettings = $state(false);
  let installMessage = $state('');
  async function showInstall() { installMessage = await installApp(); }
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
    stop(); selected = '';
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
  async function copy() { try { await navigator.clipboard.writeText(command); copied = true; } catch { copied = false; } }
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
  <main class="login-shell">
    <section class="login-card" aria-labelledby="relay-heading">
      <details class="connections-menu"><summary aria-label="Open navigation">☰</summary><nav aria-label="App actions">
        <button aria-haspopup="dialog" onclick={() => showSettings = true}>Settings</button>
        <button aria-haspopup="dialog" onclick={showInstall}>Install app</button>
      </nav></details>
      <div class="wordmark"><span class="herdr-mark" aria-hidden="true">h</span>herdr<span class="web-label">web</span></div>
      <p class="eyebrow">A private connection to your computer.</p>
      <h1 id="relay-heading">{selected ? 'Connect to your computer' : 'Connections'}</h1>
      <p role="status">{connectionState.status}</p>
      {#if connectionState.error}<p class="error" role="alert">{connectionState.error}</p>{/if}
      {#if selected}
        <p>Computer {computers.find(c => c.machineId === selected)?.name || selected.slice(0, 8)} remains selected. If authorization was revoked, scan a fresh private link from that computer to pair again.</p>
        <button onclick={retry}>Retry connection</button>
        <button onclick={showConnections}>Connections</button>
      {/if}
      {#if computers.length}
        <label for="paired-computer">Computer</label>
        <select id="paired-computer" value={selected} onchange={event => choose(event.currentTarget.value)}>
          <option value="">Choose a computer…</option>
          {#each computers as computer (computer.machineId)}<option value={computer.machineId}>{computer.name || `Computer ${computer.machineId.slice(0, 8)}`}</option>{/each}
        </select>
        {#if !selected}
          <ul class="connections">
            {#each computers as computer (computer.machineId)}
              <li><a href={connectionPath(computer.machineId)} onclick={event => { if (!event.ctrlKey && !event.metaKey && !event.shiftKey && !event.altKey && event.button === 0) { event.preventDefault(); choose(computer.machineId); } }}>{computer.name || `Computer ${computer.machineId.slice(0, 8)}`}</a></li>
            {/each}
          </ul>
        {/if}
        {#if selected}<button onclick={forget}>Forget this computer</button>{/if}
      {/if}
      <h2>Connect a computer</h2>
      <p>Run this command on the computer with Herdr, then scan the QR code it displays with your phone camera. Open its private link in this browser to pair.</p>
      <pre class="install-command"><code>{command}</code></pre>
      <button onclick={copy}>{copied ? 'Copied' : 'Copy command'}</button>
      <p class="login-footnote">Pairing pins your computer’s identity. Terminal content, commands and files travel only through encrypted WebRTC, directly when possible, or via TURN when the relay has it configured.</p>
      <details><summary>Connection diagnostics</summary><label><input type="checkbox" bind:checked={forceTurn} disabled={!!connection} /> Force TURN on the next connection</label><p>Relay {relayVersion || 'version unavailable'}. TURN availability depends on this relay’s configuration.</p></details>
    </section>
  </main>
{/if}
{#if showSettings}<SettingsDialog close={() => showSettings = false} peerMode {serverMode} connected={false} />{/if}
{#if installMessage}<InstallDialog message={installMessage} close={() => installMessage = ''} />{/if}
<style>
  .login-card { position: relative; }
  .connections-menu { position: absolute; top: 10px; right: 10px; z-index: 1; margin: 0; }
  .connections-menu summary { min-width: 44px; min-height: 44px; display: grid; place-items: center; cursor: pointer; border-radius: 6px; }
  .connections-menu nav { position: absolute; right: 0; width: 180px; padding: 6px; border: 1px solid var(--muted); border-radius: 8px; background: var(--raised); }
  .connections-menu button { display: block; text-align: left; width: 100%; min-height: 44px; }
  .revision-update { position: fixed; z-index: 1000; top: env(safe-area-inset-top, 0px); left: 50%; transform: translateX(-50%); width: min(95%, 36rem); display: flex; align-items: center; justify-content: space-between; gap: .6rem; padding: .4rem .7rem; max-height: calc(100dvh - env(safe-area-inset-top, 0px)); overflow: auto; flex-wrap: wrap; overflow-wrap: anywhere; border: 1px solid var(--muted); border-radius: .5rem; background: var(--raised); color: var(--ink); font-size: .8rem; }
  .revision-update button { min-height: 44px; flex-shrink: 0; }
  .install-command { white-space: pre-wrap; overflow-wrap: anywhere; padding: .8rem; background: var(--raised); border-radius: .5rem; font-size: .8rem; color: var(--ink); }
  select { width: 100%; padding: .7rem; margin: .4rem 0; }
  h2 { font-size: 1rem; margin-top: 1.7rem; }
  details { margin-top: 1rem; font-size: .8rem; }
  input[type=checkbox] { display: inline-block; width: auto; min-height: 0; margin-right: .4rem; }
  @media (max-width: 480px) { .login-shell { padding: 12px; } .login-card { padding: 22px; } }
</style>
