export type UpdatePhase = 'idle' | 'checking' | 'available' | 'downloading' | 'restarting' | 'error';
export interface UpdateStatus {
  current_version: string;
  available_version: string | null;
  phase: UpdatePhase;
  message: string;
  can_install: boolean;
  release_url: string | null;
  checked_at: number | null;
  source: 'release' | 'development';
}
const phases: UpdatePhase[] = ['idle', 'checking', 'available', 'downloading', 'restarting', 'error'];
// SemVer, bounded before matching; numeric identifiers cannot have leading zeroes.
const numeric = '(?:0|[1-9][0-9]*)';
const identifier = '(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)';
const versionPattern = new RegExp(`^${numeric}\\.${numeric}\\.${numeric}(?:-${identifier}(?:\\.${identifier})*)?(?:\\+[0-9A-Za-z-]+(?:\\.[0-9A-Za-z-]+)*)?$`);
export function validVersion(value: unknown): value is string {
  return typeof value === 'string' && value.length <= 128 && versionPattern.test(value);
}
export function releaseURL(version: string): string {
  return `https://github.com/perapp/herdr-webapp/releases/tag/v${version}`;
}
function hasControlCharacters(value: string): boolean {
  return Array.from(value).some(char => {
    const code = char.charCodeAt(0);
    return code === 127 || (code < 32 && code !== 9 && code !== 10 && code !== 13);
  });
}
export function parseUpdateStatus(value: unknown): UpdateStatus {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Update status is unreadable. Try checking again.');
  const v = value as Record<string, unknown>;
  if (!validVersion(v.current_version) || !(v.available_version === null || validVersion(v.available_version)) ||
      !phases.includes(v.phase as UpdatePhase) || typeof v.can_install !== 'boolean' ||
      typeof v.message !== 'string' || v.message.length > 2048 || hasControlCharacters(v.message) ||
      !(v.source === 'release' || v.source === 'development') ||
      !(v.checked_at === null || (typeof v.checked_at === 'number' && Number.isSafeInteger(v.checked_at) && v.checked_at >= 0)) ||
      !(v.release_url === null || (v.available_version !== null && v.release_url === releaseURL(v.available_version as string)))) {
    throw new Error('Update status is unreadable. Try checking again.');
  }
  return {
    current_version: v.current_version, available_version: v.available_version,
    phase: v.phase as UpdatePhase, message: v.message, can_install: v.can_install,
    release_url: v.release_url, checked_at: v.checked_at, source: v.source,
  } as UpdateStatus;
}
export function updateBusy(status: UpdateStatus | null): boolean {
  return !!status && ['checking', 'downloading', 'restarting'].includes(status.phase);
}
export function canInstallUpdate(status: UpdateStatus | null): boolean {
  return !!status && status.source === 'release' && status.can_install && !!status.available_version &&
    status.available_version !== status.current_version && !updateBusy(status);
}
export interface ReloadState { fromVersion: string | null; reloaded: boolean }
export function detectUpdatedVersion(state: ReloadState, status: UpdateStatus): boolean {
  if (state.reloaded || state.fromVersion === null || state.fromVersion === status.current_version) return false;
  state.reloaded = true;
  return true;
}
