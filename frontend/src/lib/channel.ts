/** Channel context is separate from the browser's strict HTTP Origin. */
export function channelBase(href?: string): '/' | '/main/' {
  const value = href ?? (typeof location === 'undefined' ? undefined : location.href);
  if (!value) return '/';
  const path = new URL(value).pathname;
  return path === '/main' || path.startsWith('/main/') ? '/main/' : '/';
}
export function channelPath(path: string, href?: string): string {
  if (!path.startsWith('/') || path.startsWith('//')) throw new Error('Expected an absolute channel path');
  return channelBase(href) === '/main/' && path !== '/main' && !path.startsWith('/main/') ? `/main${path}` : path;
}
export function channelContext(): string { return location.origin + (channelBase() === '/main/' ? '/main' : ''); }
/** Prevent accidental identity/UI reuse only. Shared-origin channels are NOT a security boundary. */
export function storageKey(key: string): string { return channelBase() === '/main/' ? `herdr.main:${key}` : key; }
/** Only native browser network I/O is prefixed; peer service routes stay /api/*. */
export function channelInput(input: RequestInfo | URL): RequestInfo | URL {
  if (channelBase() === '/') return input;
  if (typeof input === 'string' && input.startsWith('/') && !input.startsWith('//')) return channelPath(input);
  const url = new URL(input instanceof Request ? input.url : String(input), location.href);
  if (url.host !== location.host || !['http:', 'https:', 'ws:', 'wss:'].includes(url.protocol)) return input;
  const path = channelPath(url.pathname);
  if (path === url.pathname) return input;
  url.pathname = path;
  return input instanceof Request ? new Request(url, input) : input instanceof URL ? url : url.href;
}
