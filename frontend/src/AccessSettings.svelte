<script lang="ts">
  import { getContext, onMount, tick } from 'svelte';
  import { AccessController } from './lib/access';
  import type { AccessState } from './lib/access';
  import { captureTransportRequest } from './lib/transport';
  import { writeClipboard } from './lib/clipboard';
  import { qrImage } from './lib/qr';
  import type { QrImage } from './lib/qr';
  import { channelBase } from './lib/channel';
  import { reloadContext } from './lib/mainReload';
  import type { ReloadSafety } from './lib/mainReload';
  let { peerMode = false, connected = false, computerName = 'Connected computer' }: { peerMode?: boolean; connected?: boolean; computerName?: string } = $props();
  let access = $state<AccessState>({ status: null, pending: false, confirming: false, unavailable: false, uncertain: false, issue: '', message: '' });
  let rotateButton = $state<HTMLButtonElement>();
  let confirmButton = $state<HTMLButtonElement>();
  const safety = getContext<ReloadSafety | undefined>(reloadContext);
  const controller = new AccessController(captureTransportRequest(), value => access = value, () => safety?.busy() || (() => {}));
  const available = $derived(!peerMode || connected);
  const target = $derived(peerMode ? computerName : 'this webapp server');
  const channel = $derived(peerMode ? (channelBase() === '/main/' ? 'Main' : 'Stable') : 'Local server');
  async function ask() { controller.ask(); await tick(); if (access.confirming) confirmButton?.focus({ preventScroll: true }); }
  async function cancel() { controller.cancel(); await tick(); rotateButton?.focus({ preventScroll: true }); }
  // A browser may refuse the clipboard once the tap that started the fetch has
  // expired. The link is then held (never rendered) for one more tap, briefly.
  let held = '';
  let forget: ReturnType<typeof setTimeout> | undefined;
  function hold(link: string) {
    clearTimeout(forget); held = link;
    if (link) forget = setTimeout(() => held = '', 60_000);
  }
  // The QR is drawn in this page only while the user asked to see it.
  let qr = $state<QrImage>();
  let hideQr: ReturnType<typeof setTimeout> | undefined;
  function closeQr() { clearTimeout(hideQr); qr = undefined; }
  async function showQr() {
    const link = held || await controller.link(`${location.origin}${channelBase()}`);
    if (!link) return;
    try { qr = await qrImage(link); hideQr = setTimeout(closeQr, 120_000); }
    catch { controller.unreadable(); }
  }
  async function copyLink() {
    const link = held || await controller.link(`${location.origin}${channelBase()}`);
    if (!link) return;
    try { await writeClipboard(link); hold(''); controller.copied(true); }
    catch { hold(link); controller.copied(false); }
  }
  onMount(() => { if (available) void controller.load(); return () => { hold(''); closeQr(); controller.stop(); }; });
</script>

<div class="access-card" data-testid="access-settings">
  {#if !available}
    <p>Connect to a computer to manage its access.</p>
  {:else}
    <p class="scope"><strong>{target}</strong><span>{channel}</span></p>
    {#if access.status?.supported}
      <p>Private access links stay valid until you rotate them. Signed-in browsers do not expire automatically.</p>
      <p class="hint">The computer’s terminal hides the link and QR after a client connects. Press <kbd>L</kbd> there to show or hide them.</p>
      {#if !access.confirming}
        <div class="actions">
          <button data-testid="copy-access-link" disabled={access.pending} onclick={() => void copyLink()}>Copy access link</button>
          <button data-testid="show-access-qr" disabled={access.pending} aria-expanded={!!qr} onclick={() => qr ? closeQr() : void showQr()}>{qr ? 'Hide QR code' : 'Show QR code'}</button>
          <button bind:this={rotateButton} disabled={access.pending || !access.status.canRotate || access.uncertain} onclick={ask} aria-expanded={false}>Rotate access token…</button>
        </div>
        {#if qr}
          <figure class="access-qr">
            <svg viewBox="0 0 {qr.size} {qr.size}" role="img" aria-label="QR code of the private access link" shape-rendering="crispEdges"><rect width={qr.size} height={qr.size} fill="#fff" /><path d={qr.path} fill="#000" /></svg>
            <figcaption class="hint">Scan with the other device’s camera. Anyone who scans it gets full access; it hides itself after two minutes.</figcaption>
          </figure>
        {/if}
        <p class="hint">The link signs another browser in to {target}. It is only ever shown as this QR code, when you ask.</p>
      {:else}
        <div class="confirmation" role="group" aria-label="Confirm access token rotation">
          <h4>Rotate access for {target}?</h4>
          <p>This browser profile stays connected. All other browsers for {target}{peerMode ? ` on ${channel}` : ''} are signed out, and old private links and QR codes stop working.</p>
          <p class="hint">Core and running terminals are not stopped. {peerMode ? 'Other computers and channels are unchanged.' : 'This covers the whole webapp server, including its federated terminals. Separate servers and relay channels are unchanged.'}</p>
          <div class="actions"><button onclick={() => void cancel()}>Cancel</button><button class="danger" bind:this={confirmButton} onclick={() => void controller.rotate()} disabled={access.pending}>Rotate and sign out others</button></div>
        </div>
      {/if}
      <details><summary>About persistent access</summary><p class="hint">A private link grants trusted shell access. Keep it private and rotate it if it is shared accidentally. This browser profile includes tabs sharing its credentials. Your browser may limit cookie lifetimes or clear cookies and local storage.</p></details>
    {:else if access.pending}<p role="status">Loading access controls…</p>{/if}
    {#if access.pending && access.status}<p role="status">Rotating access token…</p>{/if}
    {#if access.message}<p class="success" role="status">{access.message}</p>{/if}
    {#if access.issue}<p role="alert">{access.issue}</p>{/if}
    {#if access.issue && !access.uncertain}<button disabled={access.pending} onclick={() => void controller.load()}>Check access</button>{/if}
  {/if}
</div>

<style>
  .access-card { border: 1px solid var(--line); background: var(--panel); border-radius: 12px; padding: 16px; min-width: 0; }
  p { margin: 0 0 12px; line-height: 1.5; overflow-wrap: anywhere; }
  .scope { display: flex; justify-content: space-between; gap: 8px; flex-wrap: wrap; }
  .scope strong { font-weight: 550; }
  .scope span, .hint { color: var(--muted); font-size: .846154rem; }
  kbd { padding: 1px 5px; border: 1px solid var(--line); border-radius: 4px; color: var(--bright); }
  button { min-height: 44px; max-width: 100%; overflow-wrap: anywhere; }
  .confirmation { border-top: 1px solid var(--line); padding-top: 14px; }
  h4 { font-size: 1rem; margin: 0 0 10px; font-weight: 600; overflow-wrap: anywhere; }
  .access-qr { margin: 0 0 12px; }
  .access-qr svg { display: block; width: min(100%, 320px); height: auto; border-radius: 8px; }
  .access-qr figcaption { margin-top: 8px; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; }
  .danger { color: var(--red, var(--bright)); border-color: var(--red, var(--line)); }
  details { margin-top: 16px; }
  summary { min-height: 44px; display: flex; align-items: center; cursor: pointer; color: var(--muted); font-size: .846154rem; }
  details p { margin: 0; }
  .success { color: var(--teal); margin-top: 12px; }
</style>
