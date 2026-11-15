import { request } from './api';
import { channelBase, storageKey } from './channel';
import { connectionRoute } from './connectionRoute';
import { record } from './schema';
import { captureTransportRequest } from './transport';
import { applicationServerKey, browserNotificationEnvironment, notificationLimitation } from './notificationSupport';

function context() {
  const route = connectionRoute();
  if (route.kind !== 'computer') throw new Error('Open a paired computer to configure notifications.');
  const base = channelBase(), id = route.id, send = captureTransportRequest();
  return { id, base, key: storageKey(`herdr.notifications.v1:${id}`), scope: `${base}notifications/${id}/`, request: (path: string, body?: unknown) => request(path, body, undefined, send) };
}
type Context = ReturnType<typeof context>;
export function notificationsEnabled(): boolean {
  try { return localStorage.getItem(context().key) === 'enabled'; } catch { return false; }
}
function preference(ctx: Context, enabled: boolean): void {
  if (enabled) localStorage.setItem(ctx.key, 'enabled');
  else localStorage.removeItem(ctx.key);
}
async function existingRegistration(ctx: Context): Promise<ServiceWorkerRegistration | undefined> {
  const expected = new URL(ctx.scope, location.origin).href;
  return (await navigator.serviceWorker.getRegistrations()).find(registration => registration.scope === expected);
}
export function notificationUnavailable(): string | undefined { return notificationLimitation(browserNotificationEnvironment()); }
export interface NotificationStatus { supported: boolean; publicKey?: string }
async function configuration(ctx: Context): Promise<NotificationStatus> {
  const config = await ctx.request('/api/notifications');
  if (!record(config) || config.supported !== true || typeof config.publicKey !== 'string') return { supported: false };
  return { supported: true, publicKey: config.publicKey };
}
export async function notificationConfiguration(): Promise<NotificationStatus> { return configuration(context()); }
export async function notificationHealth(): Promise<string> {
  const ctx = context();
  if (!notificationsEnabled()) return 'Off for this computer in this browser.';
  if (Notification.permission !== 'granted') return 'App preference is on, but browser permission is not granted.';
  const subscription = await (await existingRegistration(ctx))?.pushManager.getSubscription();
  return subscription ? 'Enabled for this computer. Browser permission granted; a push subscription exists.' : 'App preference is on, but its push subscription is missing. Enable again to repair it.';
}
async function active(registration: ServiceWorkerRegistration): Promise<void> {
  if (registration.active) return;
  const worker = registration.installing || registration.waiting;
  if (!worker) throw new Error('The notification worker is unavailable. Reload and try again.');
  await new Promise<void>((resolve, reject) => {
    const timeout = window.setTimeout(() => finish(new Error('The notification worker did not activate. Reload and try again.')), 15000);
    function finish(error?: Error) { clearTimeout(timeout); worker!.removeEventListener('statechange', changed); if (error) reject(error); else resolve(); }
    function changed() { if (worker!.state === 'activated') finish(); else if (worker!.state === 'redundant') finish(new Error('The notification worker was replaced. Try again.')); }
    worker.addEventListener('statechange', changed); changed();
  });
}
/** Permission must be requested directly in the Settings click's user gesture. */
export async function enableNotifications(): Promise<void> {
  const limitation = notificationUnavailable();
  if (limitation) throw new Error(limitation);
  const ctx = context();
  const permission = await Notification.requestPermission();
  if (permission !== 'granted') throw new Error('Browser notification permission was not granted. Enable it in browser/site and system notification settings, then try again.');
  const config = await configuration(ctx);
  if (!config.supported || !config.publicKey) throw new Error('This computer does not support background notifications. Update its connector and keep it running.');
  // Independent registrations allow remembered computers with different VAPID
  // keys to keep subscriptions simultaneously. These contain only public IDs.
  const registration = await navigator.serviceWorker.register(`${ctx.base}sw.js`, { scope: ctx.scope, updateViaCache: 'none' });
  await active(registration);
  let subscription = await registration.pushManager.getSubscription();
  const serverKey = applicationServerKey(config.publicKey);
  if (subscription?.options.applicationServerKey && !equalKey(subscription.options.applicationServerKey, serverKey)) {
    await ctx.request('/api/notifications/delete', { endpoint: subscription.endpoint });
    await subscription.unsubscribe(); subscription = null;
  }
  subscription ??= await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: serverKey });
  await ctx.request('/api/notifications', { subscription: subscription.toJSON(), connectionId: ctx.id, channel: ctx.base === '/main/' ? 'main' : 'stable' });
  try { preference(ctx, true); }
  catch {
    // Do not leave invisible delivery enabled when its browser preference cannot be stored.
    await subscription.unsubscribe();
    void ctx.request('/api/notifications/delete', { endpoint: subscription.endpoint }).catch(() => {});
    throw new Error('Browser storage is unavailable. Notifications were not enabled; allow persistent browser storage and try again.');
  }
}
function equalKey(a: ArrayBuffer, b: ArrayBuffer): boolean {
  const x = new Uint8Array(a), y = new Uint8Array(b);
  return x.length === y.length && x.every((byte, i) => byte === y[i]);
}
export async function disableNotifications(): Promise<void> {
  const ctx = context();
  const registration = await existingRegistration(ctx);
  const subscription = await registration?.pushManager.getSubscription();
  if (subscription) {
    // Do not claim server removal if the authenticated request fails.
    await ctx.request('/api/notifications/delete', { endpoint: subscription.endpoint });
    if (!await subscription.unsubscribe()) throw new Error('Computer subscription removed, but the browser could not unsubscribe. Check site settings.');
  }
  preference(ctx, false);
  await registration?.unregister();
}
const clientId = typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function' ? crypto.randomUUID() : String(Math.random()).slice(2);
/** Bind both identity and transport before any asynchronous work or navigation. */
export function notificationPresenceReporter(): (terminal: string) => Promise<void> {
  let ctx: Context;
  try { ctx = context(); } catch { return async () => {}; }
  return async terminal => {
    if (notificationUnavailable() || Notification.permission !== 'granted') return;
    try { if (localStorage.getItem(ctx.key) !== 'enabled') return; } catch { return; }
    const subscription = await (await existingRegistration(ctx))?.pushManager.getSubscription();
    if (subscription) await ctx.request('/api/notifications/seen', { endpoint: subscription.endpoint, terminal, clientId });
  };
}
/** Forgetting removes this browser's local push worker, not other device authorization. */
export async function forgetLocalNotifications(): Promise<void> {
  const ctx = context();
  try { preference(ctx, false); } catch { /* Forget must still invalidate browser push if preference storage is unavailable. */ }
  if (!('serviceWorker' in navigator)) return;
  const registration = await existingRegistration(ctx);
  const subscription = await registration?.pushManager.getSubscription();
  if (subscription) {
    // Best effort server cleanup; local forgetting must also work offline.
    try { await ctx.request('/api/notifications/delete', { endpoint: subscription.endpoint }); } catch { /* Subscription invalidated locally below. */ }
    await subscription.unsubscribe();
  }
  await registration?.unregister();
}
