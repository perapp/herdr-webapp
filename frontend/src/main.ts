import { channelPath } from './lib/channel';
import { initializePwa } from './lib/pwa';
import { mount } from 'svelte';
import App from './App.svelte';
import RelayApp from './RelayApp.svelte';
import './style.css';
import { initializeAppearance } from './lib/appearance';
import { initializeFontSize } from './lib/fontSize';
import { takePairingFragment } from './lib/peerCrypto';

// Clear pairing capabilities synchronously, before boot checks or application initialization.
let pairingFragment = takePairingFragment();
const fragment = new URLSearchParams(location.hash.slice(1));
let token = fragment.get('token') || '';
if (fragment.has('token')) history.replaceState(null, '', location.pathname + location.search);
fragment.delete('token');
initializeAppearance();
initializeFontSize();
initializePwa();

async function boot() {
  const root = document.getElementById('app')!;
  try {
    let serverMode: unknown;
    const response = await fetch(channelPath('/api/mode'), { credentials: 'same-origin', cache: 'no-store' });
    if (response.status !== 404) {
      if (!response.ok) throw new Error('Mode check failed');
      const mode = await response.json();
      serverMode = mode;
      if (mode.mode === 'relay') {
        mount(RelayApp, { target: root, props: { pairingFragment, serverMode: mode, sourceRevision: __HERDR_SOURCE_REVISION__, relayVersion: typeof mode.version === 'string' ? mode.version : '' } });
        pairingFragment = null; token = ''; return;
      }
      if (mode.mode !== 'local') throw new Error('Unsupported server mode');
    }
    pairingFragment = null;
    mount(App, { target: root, props: { initialToken: token, serverMode } }); token = '';
  } catch {
    pairingFragment = null; token = '';
    // Fail closed: do not accidentally send pairing or application traffic to a relay as local HTTP.
    const message = document.createElement('p');
    message.textContent = 'Unable to identify this Herdr server. Check your connection and reload.';
    message.setAttribute('role', 'alert'); root.replaceChildren(message);
  }
}
void boot();
