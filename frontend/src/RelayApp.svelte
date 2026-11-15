<script lang="ts">
  import { channelPath } from './lib/channel';
  import { onMount, setContext } from 'svelte';
  import { MainReload, reloadContext, trackReloadActivity } from './lib/mainReload';
  import type { ReloadState } from './lib/mainReload';
  import App from './App.svelte';
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
  const command = `curl -fsSL '${location.origin}${channelPath('/run')}' | sh`;
  async function refresh() { computers = await listComputers(); }
  function connect(target: PeerTarget) {
    connection?.stop(); setApplicationTransport(disconnectedTransport);
    selected = target.pairing?.machineId || target.computer?.machineId || '';
    connection = new PeerConnection(target, next => {
      if (!mounted) return;
      // Snapshot before App teardown; removed drafts must not become falsely safe.
      captureActivity();
      if (next.status === 'Pairing expired') paired?.();
      // Relay App is unmounted on loss: no stale terminal, HTTP or upload can replay.
      setApplicationTransport(next.service || disconnectedTransport);
      connectionState = next;
      if (next.service) { paired?.(); void refresh(); }
    }, forceTurn, () => { void updater.check(); });
    connection.start();
  }
  function choose(id: string) { const computer = computers.find(item => item.machineId === id); if (computer) connect({ computer }); }
  async function forget() {
    connection?.stop(); setApplicationTransport(disconnectedTransport); connection = undefined;
    const id = selected; selected = ''; connectionState = { status: 'Computer forgotten on this browser.' };
    await forgetComputer(id); await refresh(); paired?.();
  }
  async function copy() { try { await navigator.clipboard.writeText(command); copied = true; } catch { copied = false; } }
  onMount(() => {
    mounted = true;
    const activity = trackReloadActivity(document, updater);
    captureActivity = activity.capture;
    const foreground = () => { if (document.visibilityState === 'visible') void updater.check(); };
    document.addEventListener('visibilitychange', foreground);
    void updater.check(); // Also catch old assets racing a newly activated mode response.
    setApplicationTransport(disconnectedTransport);
    void (async () => {
      try {
        await refresh();
        if (!mounted) return;
        if (pairingFragment) {
          const fragment = pairingFragment; pairingFragment = null;
          const pairing = await parsePairing(fragment);
          if (mounted) connect({ pairing });
        } else if (computers.length === 1) choose(computers[0].machineId);
      } catch { paired?.(); connectionState = { status: 'Unable to pair', error: 'The pairing link is invalid or expired, or secure browser storage is unavailable. Scan a fresh QR code.' }; }
    })();
    return () => { mounted = false; updater.stop(); activity.stop(); document.removeEventListener('visibilitychange', foreground); connection?.stop(); setApplicationTransport(disconnectedTransport); };
  });
</script>

{#if update.ready}
  <aside class="revision-update" role="status">
    <span>Update ready{update.critical ? ' · Finish pairing, upload or text composition first.' : update.dirty ? ' · Reload discards unsent form drafts and selected files.' : ''}</span>
    <button disabled={update.critical} onclick={reloadUpdate}>Reload</button>
  </aside>
{/if}
{#if connectionState.service}
  <App {serverMode} peerBuild={connectionState.build} peerMode={true} peerVersion={connectionState.version} {relayVersion} peerRoute={connectionState.route} onForget={forget} computers={computers.map(c => ({ id: c.machineId, name: c.name || `Computer ${c.machineId.slice(0, 8)}` }))} selectedComputer={selected} onChooseComputer={choose} />
{:else}
  <main class="login-shell">
    <section class="login-card" aria-labelledby="relay-heading">
      <div class="wordmark"><span class="herdr-mark" aria-hidden="true">h</span>herdr<span class="web-label">web</span></div>
      <p class="eyebrow">A private connection to your computer.</p>
      <h1 id="relay-heading">Open your spaces.</h1>
      <p role="status">{connectionState.status}</p>
      {#if connectionState.error}<p class="error" role="alert">{connectionState.error}</p>{/if}
      {#if computers.length}
        <label for="paired-computer">Paired computer</label>
        <select id="paired-computer" value={selected} onchange={event => choose(event.currentTarget.value)}>
          <option value="">Choose a computer…</option>
          {#each computers as computer (computer.machineId)}<option value={computer.machineId}>{computer.name || `Computer ${computer.machineId.slice(0, 8)}`}</option>{/each}
        </select>
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
<style>
  .revision-update { position: fixed; z-index: 1000; top: env(safe-area-inset-top, 0px); left: 50%; transform: translateX(-50%); width: min(95%, 36rem); display: flex; align-items: center; justify-content: space-between; gap: .6rem; padding: .4rem .7rem; max-height: calc(100dvh - env(safe-area-inset-top, 0px)); overflow: auto; flex-wrap: wrap; overflow-wrap: anywhere; border: 1px solid var(--muted); border-radius: .5rem; background: var(--raised); color: var(--ink); font-size: .8rem; }
  .revision-update button { min-height: 44px; flex-shrink: 0; }
  .install-command { white-space: pre-wrap; overflow-wrap: anywhere; padding: .8rem; background: var(--raised); border-radius: .5rem; font-size: .8rem; color: var(--ink); }
  select { width: 100%; padding: .7rem; margin: .4rem 0; }
  h2 { font-size: 1rem; margin-top: 1.7rem; }
  details { margin-top: 1rem; font-size: .8rem; }
  input[type=checkbox] { display: inline-block; width: auto; min-height: 0; margin-right: .4rem; }
  @media (max-width: 480px) { .login-shell { padding: 12px; } .login-card { padding: 22px; } }
</style>
