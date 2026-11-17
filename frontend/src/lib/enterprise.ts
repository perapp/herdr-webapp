// Enterprise (Okta) sign-in for the relay application. The relay and connector
// enforce authorization; this module only gets the user signed in and explains
// refusals. It never weakens what the pairing link protects: the capability is
// kept in this tab's sessionStorage across the Okta redirect, never sent to a
// server, and discarded after a short time.
import { channelPath, storageKey } from './channel';

export interface EnterpriseSession { authenticated: boolean; name?: string }
export type EnterpriseDenial = 'enterprise-login-required' | 'enterprise-okta-unavailable' | 'enterprise-owner-mismatch' | 'enterprise-revoked';

const PAIRING_KEY = 'herdr.enterprise.pairing';
const ATTEMPT_KEY = 'herdr.enterprise.login-attempt';
/** A pairing link waits at most this long for the Okta sign-in to finish. */
export const PAIRING_STASH_MS = 10 * 60_000;
/** Never redirect to Okta in a loop: one automatic attempt per tab per minute. */
export const LOGIN_RETRY_MS = 60_000;

type Store = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
function store(): Store | undefined { try { return sessionStorage; } catch { return undefined; } }

export function enterpriseRequired(mode: unknown): boolean {
  return !!mode && typeof mode === 'object' && (mode as { enterprise?: { required?: unknown } }).enterprise?.required === true;
}
export function isEnterpriseDenial(code: unknown): code is EnterpriseDenial {
  return code === 'enterprise-login-required' || code === 'enterprise-okta-unavailable' || code === 'enterprise-owner-mismatch' || code === 'enterprise-revoked';
}
export function denialMessage(code: EnterpriseDenial): { status: string; error: string } {
  switch (code) {
    case 'enterprise-login-required': return { status: 'Sign-in required', error: 'Your company sign-in has ended. Sign in again to continue; nothing was replayed.' };
    case 'enterprise-okta-unavailable': return { status: 'Sign-in could not be verified', error: 'Okta could not be reached to confirm your access, so the connection was closed. Retry in a moment.' };
    case 'enterprise-owner-mismatch': return { status: 'Not your computer', error: 'This computer is enrolled to a different company account. Only its owner can connect.' };
    case 'enterprise-revoked': return { status: 'Access revoked', error: 'An administrator revoked access for this account, computer or browser. Contact your administrator.' };
  }
}
export function loginErrorMessage(code: string): string {
  switch (code) {
    case 'denied': return 'Okta did not approve the sign-in. Check that the application is assigned to you, then try again.';
    case 'expired': return 'The sign-in took too long or was started in another browser. Try again.';
    case 'rejected': return 'Okta’s response could not be validated or did not meet the required sign-in policy. Try again.';
    case 'configuration': return 'Okta did not issue a refresh token. An administrator must enable the Refresh Token grant for this application.';
    case 'revoked': return 'An administrator revoked access for this account.';
    default: return 'Okta could not be reached. Access stays closed until sign-in can be verified; try again shortly.';
  }
}

export async function enterpriseSession(fetcher: typeof fetch = fetch): Promise<EnterpriseSession> {
  const response = await fetcher(channelPath('/enterprise/session'), { credentials: 'same-origin', cache: 'no-store' });
  if (response.status === 401) return { authenticated: false };
  if (!response.ok) throw new Error('Session check failed');
  const body = await response.json();
  return { authenticated: body?.authenticated === true, name: typeof body?.name === 'string' ? body.name.slice(0, 128) : undefined };
}
export async function enterpriseLogout(fetcher: typeof fetch = fetch): Promise<void> {
  const response = await fetcher(channelPath('/enterprise/logout'), { method: 'POST', credentials: 'same-origin', cache: 'no-store' });
  if (!response.ok) throw new Error('Sign-out failed');
}

/** Keep a scanned pairing link for this tab only while the user signs in. */
export function stashPairing(fragment: string, now = Date.now(), storage = store()): void {
  try { storage?.setItem(storageKey(PAIRING_KEY), JSON.stringify({ fragment, at: now })); } catch { /* sign-in still works; pairing is repeated */ }
}
/** One use. `expired` asks the user to scan a fresh QR rather than extending it. */
export function takeStashedPairing(now = Date.now(), storage = store()): { fragment?: string; expired?: boolean } {
  let raw: string | null = null;
  try { raw = storage?.getItem(storageKey(PAIRING_KEY)) ?? null; storage?.removeItem(storageKey(PAIRING_KEY)); } catch { return {}; }
  if (!raw) return {};
  try {
    const value = JSON.parse(raw);
    if (typeof value?.fragment !== 'string' || typeof value?.at !== 'number' || value.fragment.length > 4096) return {};
    if (now < value.at || now - value.at > PAIRING_STASH_MS) return { expired: true };
    return { fragment: value.fragment };
  } catch { return {}; }
}
/** Read and remove the relay's post-login error marker from the address bar. */
export function takeLoginError(): string | null {
  const url = new URL(location.href);
  const code = url.searchParams.get('enterprise_error');
  if (code === null) return null;
  url.searchParams.delete('enterprise_error');
  history.replaceState(history.state, '', url.pathname + url.search);
  return /^[a-z-]{1,32}$/.test(code) ? code : 'unavailable';
}
export function loginUrl(path = location.pathname + location.search): string {
  return `${channelPath('/enterprise/login')}?return=${encodeURIComponent(path)}`;
}
/** False when an automatic redirect was just tried: show a button instead. */
export function mayAutoLogin(now = Date.now(), storage = store()): boolean {
  try {
    const last = Number(storage?.getItem(storageKey(ATTEMPT_KEY)) ?? 0);
    if (Number.isFinite(last) && last > 0 && now >= last && now - last < LOGIN_RETRY_MS) return false;
    storage?.setItem(storageKey(ATTEMPT_KEY), String(now));
    return !!storage;
  } catch { return false; }
}
export function clearLoginAttempt(storage = store()): void {
  try { storage?.removeItem(storageKey(ATTEMPT_KEY)); } catch { /* nothing to clear */ }
}
