/* Push-only worker: no fetch interception, caching or forced activation/reload. */
'use strict';
const eventTitles = Object.freeze({ done: 'Agent finished', blocked: 'Agent needs input', error: 'Agent reported an error' });
const identifier = value => typeof value === 'string' && /^[A-Za-z0-9_-]{1,128}$/.test(value);
const terminalAddress = value => typeof value === 'string' && value.length <= 512 && /^[A-Za-z0-9][A-Za-z0-9_.-]*(?:\/[A-Za-z0-9][A-Za-z0-9_.-]*)*$/.test(value);
function context() {
  const script = new URL(self.location.href);
  const base = script.pathname === '/sw.js' ? '/' : script.pathname === '/main/sw.js' ? '/main/' : null;
  if (!base) return null;
  const scope = new URL(self.registration.scope);
  if (scope.origin !== script.origin) return null;
  if (scope.pathname === base) return { base, origin: script.origin, connection: null };
  const prefix = `${base}notifications/`;
  if (!scope.pathname.startsWith(prefix) || !scope.pathname.endsWith('/')) return null;
  const connection = scope.pathname.slice(prefix.length, -1);
  return identifier(connection) ? { base, origin: script.origin, connection } : null;
}
function destination(payload) {
  if (!payload || typeof payload !== 'object' || Array.isArray(payload)) return null;
  const ctx = context();
  if (!ctx || payload.version !== 1 || !identifier(payload.eventId) || typeof payload.kind !== 'string' || !Object.hasOwn(eventTitles, payload.kind)
      || !identifier(payload.connectionId) || !terminalAddress(payload.terminal)) return null;
  if (Object.keys(payload).some(key => !['version', 'eventId', 'kind', 'connectionId', 'channel', 'terminal'].includes(key))) return null;
  if (payload.channel !== (ctx.base === '/' ? 'stable' : 'main')) return null;
  if (ctx.connection && payload.connectionId !== ctx.connection) return null;
  const url = new URL(`${ctx.base}connections/${payload.connectionId}`, ctx.origin);
  url.searchParams.set('terminal', payload.terminal);
  return url.href;
}
function validatedClick(value) {
  if (typeof value !== 'string') return null;
  try {
    const ctx = context();
    if (!ctx) return null;
    const url = new URL(value);
    if (url.origin !== ctx.origin || url.username || url.password || url.hash) return null;
    const prefix = `${ctx.base}connections/`;
    if (!url.pathname.startsWith(prefix)) return null;
    const connection = url.pathname.slice(prefix.length);
    if (!identifier(connection) || (ctx.connection && connection !== ctx.connection)) return null;
    const entries = [...url.searchParams];
    if (entries.length !== 1 || entries[0][0] !== 'terminal' || !terminalAddress(entries[0][1])) return null;
    const expected = new URL(`${prefix}${connection}`, ctx.origin);
    expected.searchParams.set('terminal', entries[0][1]);
    return expected.href === url.href ? url.href : null;
  } catch { return null; }
}
self.addEventListener('push', event => {
  let payload;
  try { payload = event.data?.json(); } catch { return; }
  const url = destination(payload);
  if (!url) return;
  const ctx = context();
  event.waitUntil(self.registration.showNotification(eventTitles[payload.kind], {
    body: 'Open Herdr to view this agent.',
    icon: `${ctx.base}icons/icon-192.png`,
    badge: `${ctx.base}icons/icon-192.png`,
    tag: `herdr-${payload.connectionId}-${payload.eventId}`,
    data: { url },
  }));
});
self.addEventListener('notificationclick', event => {
  event.notification.close();
  const url = validatedClick(event.notification.data?.url);
  if (!url) return;
  event.waitUntil((async () => {
    const clients = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
    // Reuse only an exact destination. Never redirect another active computer's terminal.
    const existing = clients.find(client => client.url === url);
    if (existing) { await existing.focus(); return; }
    await self.clients.openWindow(url);
  })());
});
