import { channelBase, channelPath } from './channel';

interface InstallPrompt extends Event {
  prompt(): Promise<void>;
  userChoice: Promise<{ outcome: 'accepted' | 'dismissed' }>;
}
let pending: InstallPrompt | undefined;
let initialized = false;

/** No reload on controllerchange: updating a worker must not interrupt terminals. */
export function initializePwa(): void {
  if (initialized) return;
  initialized = true;
  window.addEventListener('beforeinstallprompt', event => {
    event.preventDefault(); pending = event as InstallPrompt;
  });
  window.addEventListener('appinstalled', () => { pending = undefined; });
  const base = channelBase();
  const manifest = document.createElement('link');
  manifest.rel = 'manifest'; manifest.href = channelPath('/manifest.webmanifest');
  const icon = document.createElement('link');
  icon.rel = 'apple-touch-icon'; icon.href = channelPath('/icons/icon-192.png');
  const favicon = document.createElement('link');
  favicon.rel = 'icon'; favicon.href = channelPath('/icons/herdr.svg');
  document.head.append(manifest, icon, favicon);
  if (window.isSecureContext && 'serviceWorker' in navigator) {
    // No authenticated URL, query or fragment is ever passed to registration.
    void navigator.serviceWorker.register(`${base}sw.js`, { scope: base, updateViaCache: 'none' }).catch(() => {
      // Installation remains usable without background capabilities.
    });
  }
}

export interface InstallEnvironment {
  secure: boolean; standalone: boolean; userAgent: string; platform: string; touchPoints: number;
}
export function installExplanation(env: InstallEnvironment): string {
  if (env.standalone) return 'Herdr is already opened as an app. You can keep using this window; this browser does not expose all installations in other windows.';
  if (!env.secure) return 'App installation requires a secure connection. Open Herdr using its HTTPS address in a supported full browser.';
  const ua = env.userAgent;
  if (/\bwv\b|; wv\)|FBAN|FBAV|Instagram|Line\//i.test(ua)) return 'This appears to be an embedded browser, where installation may be unavailable. Open this address in Safari, Chrome, or another supported full browser.';
  const apple = /iPad|iPhone|iPod/i.test(ua) || (env.platform === 'MacIntel' && env.touchPoints > 1);
  if (apple) return 'On iPhone or iPad, use the browser’s Share menu → Add to Home Screen. If that option is missing, open this address in Safari and try there. A programmatic install prompt is not available here.';
  if (/Android/i.test(ua)) return 'Open your browser’s menu and look for Install app or Add to Home screen. Chrome and Samsung Internet support installation, but wording and availability vary. If absent, try a supported full browser.';
  if (/Firefox/i.test(ua)) return 'This Firefox window has not offered an install prompt. Desktop Firefox may not support installing this app; try Chrome or Edge, or continue using Herdr in this browser.';
  if (/Chrome|Chromium|Edg\//i.test(ua)) return 'The browser has not offered an install prompt. Check its menu or address bar for Install app. It may already be installed, or browser policy or install requirements may prevent installation. You can continue using Herdr here.';
  if (/Safari/i.test(ua)) return 'In supported Safari versions on Mac, use File → Add to Dock. If unavailable, update Safari or use another supported browser. You can continue using Herdr here.';
  return 'This browser has not offered an install prompt; the reason is unknown. Check its menu for an install option, try a supported browser, or continue using Herdr here.';
}
export async function installApp(): Promise<string> {
  const standalone = matchMedia('(display-mode: standalone)').matches || (navigator as Navigator & { standalone?: boolean }).standalone === true;
  if (!standalone && pending) {
    const prompt = pending; pending = undefined;
    try {
      // Called synchronously from the click handler, before the first await.
      await prompt.prompt();
      const choice = await prompt.userChoice;
      return choice.outcome === 'accepted' ? 'Installation accepted. Your browser will finish adding Herdr.' : 'Installation dismissed. You can try again from the browser’s install menu, or return here when another prompt is available.';
    } catch { return 'The browser could not start installation. Try its Install app menu option, or open Herdr in a supported full browser.'; }
  }
  return installExplanation({ secure: window.isSecureContext, standalone, userAgent: navigator.userAgent, platform: navigator.platform, touchPoints: navigator.maxTouchPoints });
}
