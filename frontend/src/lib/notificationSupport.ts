/** Installation is a prerequisite only on Apple mobile platforms, not Android. */
export interface NotificationEnvironment {
  secure: boolean; notification: boolean; serviceWorker: boolean; push: boolean;
  standalone: boolean; userAgent: string; platform: string; maxTouchPoints: number;
}
export function notificationLimitation(env: NotificationEnvironment): string | undefined {
  if (!env.secure) return 'Notifications need a secure HTTPS connection. Open Herdr in a supported full browser over HTTPS.';
  const appleMobile = /iPhone|iPad|iPod/i.test(env.userAgent) || (env.platform === 'MacIntel' && env.maxTouchPoints > 1);
  if (appleMobile && !env.standalone) return 'On iPhone or iPad, add Herdr to your Home Screen, then open the app there to enable notifications (iOS/iPadOS 16.4 or later).';
  if (!env.notification || !env.serviceWorker || !env.push) return 'Web Push is unavailable in this browser or restricted environment. Try an up-to-date full browser; support and system policies vary.';
}
export function browserNotificationEnvironment(): NotificationEnvironment {
  return {
    secure: window.isSecureContext,
    notification: 'Notification' in window,
    serviceWorker: 'serviceWorker' in navigator,
    push: 'PushManager' in window,
    standalone: matchMedia('(display-mode: standalone)').matches || (navigator as Navigator & { standalone?: boolean }).standalone === true,
    userAgent: navigator.userAgent, platform: navigator.platform, maxTouchPoints: navigator.maxTouchPoints,
  };
}
export function applicationServerKey(value: string): ArrayBuffer {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) throw new Error('The computer returned an invalid notification key.');
  const bytes = Uint8Array.from(atob(value.replace(/-/g, '+').replace(/_/g, '/')), char => char.charCodeAt(0));
  if (bytes.length !== 65 || bytes[0] !== 4) throw new Error('The computer returned an invalid notification key.');
  return bytes.buffer;
}
export const notificationPermissionHelp = 'If blocked, enable notifications in your browser’s site permissions and your system’s notification settings. On Android, also check the browser’s app notification settings. Delivery is best-effort: connectivity, Focus modes, battery restrictions, force-stopping the browser and vendor power management can delay or prevent it.';
