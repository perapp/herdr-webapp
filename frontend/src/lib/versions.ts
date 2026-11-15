import { channelBase, channelPath } from './channel.ts';
import { validVersion } from './updates.ts';

export interface BuildInfo { version: string | null; releaseVersion?: string; revision: string | null; commitDate: string | null; dirty: boolean | null }
export function versionText(value: unknown): string | null {
  return typeof value === 'string' && value.length > 0 && value.length <= 160 && !Array.from(value).some(char => char.charCodeAt(0) < 32 || char.charCodeAt(0) === 127) ? value : null;
}
export function commitDate(value: unknown): string | null {
  if (typeof value !== 'string' || value.length > 64) return null;
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(Z|[+-](\d{2}):(\d{2}))$/.exec(value);
  if (!m || +m[2] < 1 || +m[2] > 12 || +m[3] < 1 || +m[3] > new Date(Date.UTC(+m[1], +m[2], 0)).getUTCDate() || +m[4] > 23 || +m[5] > 59 || +m[6] > 59 || +(m[8] || 0) > 23 || +(m[9] || 0) > 59 || !Number.isFinite(Date.parse(value))) return null;
  return value;
}
/** Commit-local wall time: deliberately never convert to the browser timezone. */
export function timestampVersion(revision: unknown, date: unknown): string | null {
  if (typeof revision !== 'string' || !/^[a-fA-F0-9]{40}$/.test(revision) || !commitDate(date)) return null;
  const iso = date as string;
  const zone = iso.endsWith('Z') ? '+0000' : iso.slice(-6).replace(':', '');
  return `${iso.slice(2, 4)}${iso.slice(5, 7)}${iso.slice(8, 10)}${iso.slice(11, 13)}${iso.slice(14, 16)}${iso.slice(17, 19)}${zone}-${revision.slice(0, 7).toLowerCase()}`;
}
/** Shared by menus, diagnostics and version rows; metadata beats legacy fallback. */
export function displayVersion(build: unknown, fallbackVersion?: unknown): string {
  return normalizeBuild(build, fallbackVersion).version || 'Unavailable';
}
export function normalizeBuild(value: unknown, fallbackVersion?: unknown): BuildInfo {
  const v = value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
  const revision = typeof v.revision === 'string' && /^[a-fA-F0-9]{40}$/.test(v.revision) ? v.revision.toLowerCase() : null;
  const date = revision ? commitDate(v.commitDate) : null;
  const release = v.dirty === true ? null : validVersion(v.releaseVersion) ? v.releaseVersion : null;
  // New producers attest exact tags separately. Legacy producers with a known
  // commit cannot establish that their manifest version was actually released.
  const version = release || (revision ? timestampVersion(revision, date) : versionText(v.version) || versionText(fallbackVersion));
  return { version, ...(release ? { releaseVersion: release } : {}), revision, commitDate: date, dirty: revision && typeof v.dirty === 'boolean' ? v.dirty : null };
}
export function modeBuild(value: unknown): BuildInfo {
  const v = value && typeof value === 'object' ? value as Record<string, unknown> : {};
  // Legacy revision/buildRevision fields do not establish complete build provenance.
  return normalizeBuild(v.build, v.version);
}
export function loadedBuild(): BuildInfo {
  return normalizeBuild({ version: __HERDR_APP_VERSION__, releaseVersion: typeof __HERDR_RELEASE_VERSION__ === 'undefined' ? null : __HERDR_RELEASE_VERSION__, revision: __HERDR_SOURCE_REVISION__, commitDate: __HERDR_SOURCE_COMMIT_DATE__, dirty: __HERDR_SOURCE_DIRTY__ });
}
export function displayDate(iso: string): string {
  return new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'long' }).format(new Date(iso));
}
/** Display the actual browser-facing channel endpoint, never a private access link. */
export function versionUrl(href?: string): string | undefined {
  try {
    const value = href ?? (typeof location === 'undefined' ? undefined : location.href);
    if (!value) return;
    const url = new URL(value);
    if (url.protocol !== 'https:' && url.protocol !== 'http:') return;
    // Ignore credentials, arbitrary paths, query parameters and fragments. The
    // selected connector and Core use native transports, not this HTTP endpoint.
    return url.origin + channelBase(value);
  } catch { return; }
}
export interface VersionRow { label: string; detail: string; build: BuildInfo; url?: string; unused?: boolean; core?: boolean }
export function versionRows(peer: boolean, served: unknown, connector: unknown, connectorVersion: unknown, coreVersion: unknown, connected: boolean, href?: string): VersionRow[] {
  const url = versionUrl(href);
  return [
    { label: 'Browser app', detail: 'Loaded in this browser', build: loadedBuild(), url },
    { label: 'Webapp', detail: peer ? 'Native connector · selected computer' : 'Local server', build: peer ? normalizeBuild(connector, connectorVersion) : modeBuild(served), url: peer ? undefined : url },
    { label: 'Relay', detail: peer ? 'Current backend serving this channel' : 'Not used (local connection)', build: peer ? modeBuild(served) : normalizeBuild(null), unused: !peer, url: peer ? url : undefined },
    { label: 'Herdr Core', detail: `Root Core of the selected computer${connected ? '' : ' · disconnected, last observed'}`, build: normalizeBuild(null, coreVersion), core: true },
  ];
}
/** Native network fetch, deliberately never the selected computer's transport. */
export async function fetchVersionMode(signal: AbortSignal): Promise<unknown> {
  const response = await fetch(channelPath('/api/mode'), { credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal });
  if (!response.ok) throw new Error('Version information could not be refreshed.');
  const value: unknown = await response.json();
  if (!value || typeof value !== 'object' || !['local', 'relay'].includes(String((value as Record<string, unknown>).mode))) throw new Error('Invalid version information.');
  return value;
}
