import { record } from './schema';
import { channelBase } from './channel';
import { connectionRoute } from './connectionRoute';

export type ComponentName = 'connector' | 'core';
export type ComponentState = 'unchecked' | 'checking' | 'current' | 'available' | 'updating' | 'reconnecting' | 'error' | 'unavailable';
export interface ComponentUpdate {
  state: ComponentState; currentVersion: string | null; availableVersion: string | null;
  identity: string | null; channel: string | null; managed: string | null;
  instruction: string | null; error: string | null; canCheck: boolean; canInstall: boolean;
  computer?: string | null; availableRevision?: string | null;
}
export interface ComponentUpdates { connector: ComponentUpdate; core: ComponentUpdate }
const states = ['unchecked', 'checking', 'current', 'available', 'updating', 'reconnecting', 'error', 'unavailable'];
function text(value: unknown, limit: number, lines = false): value is string | null {
  return value === null || (typeof value === 'string' && value.length <= limit && !Array.from(value).some(c => c.charCodeAt(0) === 127 || (c.charCodeAt(0) < 32 && !(lines && ['\n', '\t', '\r'].includes(c)))));
}
export function parseComponent(value: unknown): ComponentUpdate {
  if (!record(value) || !states.includes(String(value.state)) ||
      !text(value.currentVersion, 160) || !text(value.availableVersion, 160) ||
      !text(value.identity, 1024) || !text(value.channel, 80) ||
      !text(value.instruction, 4096, true) || !text(value.error, 4096, true) ||
      !text(value.managed, 160) || typeof value.canCheck !== 'boolean' || typeof value.canInstall !== 'boolean' ||
      (value.canInstall && (!value.identity || !value.availableVersion)) ||
      (value.computer !== undefined && !text(value.computer, 240)) ||
      (value.availableRevision !== undefined && value.availableRevision !== null && (typeof value.availableRevision !== 'string' || !/^[a-f0-9]{40}$/.test(value.availableRevision)))) {
    throw new Error('Update information is unreadable. Check again or update the connector on that computer.');
  }
  return value as unknown as ComponentUpdate;
}
export function parseComponentUpdates(value: unknown): ComponentUpdates {
  if (!record(value)) throw new Error('Update information is unreadable. Check again.');
  return { connector: parseComponent(value.connector), core: parseComponent(value.core) };
}
export function componentBusy(value: ComponentUpdate): boolean { return ['checking', 'updating', 'reconnecting'].includes(value.state); }
export function componentCandidate(value: ComponentUpdate): string {
  if (value.channel === 'main' && value.availableRevision) return `Main build ${value.availableRevision.slice(0, 7)}`;
  return value.availableVersion || 'Unavailable';
}
export function canUpdateComponent(value: ComponentUpdate): boolean {
  return value.state === 'available' && value.canInstall && !!value.identity && !!value.availableVersion;
}
export function updateScope(computer?: string): string {
  const route = connectionRoute();
  return channelBase() + (computer || (route.kind === 'computer' ? route.id : 'local'));
}
export interface UpdateNotice {
  component: ComponentName; from: string | null; candidate: string; started: number;
  state: 'pending' | 'done' | 'error'; message: string;
}
const notices = new Map<string, UpdateNotice>();
const listeners = new Set<() => void>();
export function updateNotice(scope: string): UpdateNotice | undefined { return notices.get(scope); }
export function watchUpdateNotices(listener: () => void): () => void { listeners.add(listener); return () => listeners.delete(listener); }
function publish(): void { for (const listener of listeners) listener(); }
/** Window-local progress only: no credentials, persisted commands or replayable requests. */
export function noteUpdate(scope: string, component: ComponentName, value: ComponentUpdate): void {
  notices.set(scope, { component, from: value.currentVersion, candidate: componentCandidate(value), started: Date.now(), state: 'pending', message: 'Update requested. Waiting for this computer…' }); publish();
}
export function clearUpdateNotice(scope: string): void { notices.delete(scope); publish(); }
export function settleUpdateNotice(scope: string, value: ComponentUpdate): void {
  const notice = notices.get(scope);
  if (!notice || notice.state !== 'pending') return;
  if (value.state === 'error') { notice.state = 'error'; notice.message = value.error || 'Update failed. Open Versions and check again.'; }
  else if (value.currentVersion && notice.from && value.currentVersion !== notice.from) { notice.state = 'done'; notice.message = `${notice.component === 'core' ? 'Herdr Core' : 'Connector'} updated to ${value.currentVersion}. Your terminals are still running.`; }
  else if (Date.now() - notice.started > 120_000 && !componentBusy(value)) { notice.state = 'error'; notice.message = 'The update could not be confirmed. Open Versions and check again; the request will not be replayed.'; }
  else return;
  publish();
}
