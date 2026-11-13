<script lang="ts">
  import { onMount } from 'svelte';
  import App from './App.svelte';
  import { parsePairing, listComputers, forgetComputer } from './lib/peerCrypto';
  import type { Computer } from './lib/peerCrypto';
  import { PeerConnection } from './lib/peerConnection';
  import type { PeerTarget, ConnectionState } from './lib/peerConnection';
  import { disconnectedTransport, setApplicationTransport } from './lib/transport';

  let { pairingFragment = null, relayVersion = '' }: { pairingFragment?: string | null; relayVersion?: string } = $props();
  let computers = $state<Computer[]>([]);
  let connectionState = $state<ConnectionState>({ status: 'Your terminals, wherever you are.' });
  let selected = $state('');
  let forceTurn = $state(new URLSearchParams(location.search).get('forceTurn') === '1');
  let connection = $state<PeerConnection>();
  let mounted = true;
  let copied = $state(false);
  const command = `curl -fsSL '${location.origin}/run' | sh`;
  async function refresh() { computers = await listComputers(); }
  function connect(target: PeerTarget) {
    connection?.stop(); setApplicationTransport(disconnectedTransport);
    selected = target.pairing?.machineId || target.computer?.machineId || '';
    connection = new PeerConnection(target, next => {
      if (!mounted) return;
      // Relay App is unmounted on loss: no stale terminal, HTTP or upload can replay.
      setApplicationTransport(next.service || disconnectedTransport);
      connectionState = next;
      if (next.service) void refresh();
    }, forceTurn);
    connection.start();
  }
  function choose(id: string) { const computer = computers.find(item => item.machineId === id); if (computer) connect({ computer }); }
  async function forget() {
    connection?.stop(); setApplicationTransport(disconnectedTransport); connection = undefined;
    const id = selected; selected = ''; connectionState = { status: 'Computer forgotten on this browser.' };
    await forgetComputer(id); await refresh();
  }
  async function copy() { try { await navigator.clipboard.writeText(command); copied = true; } catch { copied = false; } }
  onMount(() => {
    mounted = true;
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
      } catch { connectionState = { status: 'Unable to pair', error: 'The pairing link is invalid or expired, or secure browser storage is unavailable. Scan a fresh QR code.' }; }
    })();
    return () => { mounted = false; connection?.stop(); setApplicationTransport(disconnectedTransport); };
  });
</script>

{#if connectionState.service}
  <App peerMode={true} peerVersion={connectionState.version} {relayVersion} peerRoute={connectionState.route} onForget={forget} computers={computers.map(c => ({ id: c.machineId, name: c.name || `Computer ${c.machineId.slice(0, 8)}` }))} selectedComputer={selected} onChooseComputer={choose} />
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
  .install-command { white-space: pre-wrap; overflow-wrap: anywhere; padding: .8rem; background: var(--raised); border-radius: .5rem; font-size: .8rem; color: var(--ink); }
  select { width: 100%; padding: .7rem; margin: .4rem 0; }
  h2 { font-size: 1rem; margin-top: 1.7rem; }
  details { margin-top: 1rem; font-size: .8rem; }
  input[type=checkbox] { display: inline-block; width: auto; min-height: 0; margin-right: .4rem; }
  @media (max-width: 480px) { .login-shell { padding: 12px; } .login-card { padding: 22px; } }
</style>
