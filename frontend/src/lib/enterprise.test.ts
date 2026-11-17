import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { PAIRING_STASH_MS, LOGIN_RETRY_MS, clearLoginAttempt, denialMessage, enterpriseRequired, enterpriseSession, isEnterpriseDenial, loginErrorMessage, loginUrl, mayAutoLogin, stashPairing, takeLoginError, takeStashedPairing } from './enterprise';

function memory() {
  const data = new Map<string, string>();
  return { data, getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => { data.set(key, value); }, removeItem: (key: string) => { data.delete(key); } };
}
describe('enterprise sign-in helpers', () => {
  const replaceState = vi.fn();
  beforeEach(() => {
    vi.stubGlobal('location', new URL('https://relay.example/connections/abc?forceTurn=1&enterprise_error=denied'));
    vi.stubGlobal('history', { state: null, replaceState });
  });
  afterEach(() => { vi.unstubAllGlobals(); replaceState.mockReset(); });

  it('detects the mode only from an explicit requirement', () => {
    expect(enterpriseRequired({ mode: 'relay', enterprise: { required: true } })).toBe(true);
    for (const mode of [undefined, null, {}, { enterprise: null }, { enterprise: { required: 'true' } }, { enterprise: {} }]) expect(enterpriseRequired(mode)).toBe(false);
  });
  it('keeps a pairing link once, in this tab only, and never past its window', () => {
    const storage = memory();
    stashPairing('#pair=secret', 1000, storage);
    expect(takeStashedPairing(1000 + PAIRING_STASH_MS, storage)).toEqual({ fragment: '#pair=secret' });
    expect(takeStashedPairing(2000, storage)).toEqual({});
    stashPairing('#pair=secret', 1000, storage);
    expect(takeStashedPairing(1001 + PAIRING_STASH_MS, storage)).toEqual({ expired: true });
    expect(storage.data.size).toBe(0);
    stashPairing('#pair=secret', 5000, storage);
    expect(takeStashedPairing(4000, storage)).toEqual({ expired: true });
    storage.setItem('herdr.enterprise.pairing', '{broken');
    expect(takeStashedPairing(1, storage)).toEqual({});
    expect(takeStashedPairing(1, undefined)).toEqual({});
  });
  it('removes the login error marker and builds a path-only return target', () => {
    expect(takeLoginError()).toBe('denied');
    expect(replaceState).toHaveBeenCalledWith(null, '', '/connections/abc?forceTurn=1');
    vi.stubGlobal('location', new URL('https://relay.example/?enterprise_error=%3Cscript%3E'));
    expect(takeLoginError()).toBe('unavailable');
    vi.stubGlobal('location', new URL('https://relay.example/connections/abc?forceTurn=1'));
    expect(takeLoginError()).toBeNull();
    expect(loginUrl()).toBe('/enterprise/login?return=%2Fconnections%2Fabc%3FforceTurn%3D1');
    expect(loginUrl()).not.toContain('#');
  });
  it('redirects automatically at most once per window', () => {
    const storage = memory();
    expect(mayAutoLogin(1000, storage)).toBe(true);
    expect(mayAutoLogin(1000 + LOGIN_RETRY_MS - 1, storage)).toBe(false);
    expect(mayAutoLogin(1000 + LOGIN_RETRY_MS, storage)).toBe(true);
    clearLoginAttempt(storage);
    expect(mayAutoLogin(1000 + LOGIN_RETRY_MS + 1, storage)).toBe(true);
    expect(mayAutoLogin(1, undefined)).toBe(false);
  });
  it('reads the session without trusting malformed responses', async () => {
    const reply = (status: number, body: unknown) => vi.fn(async () => new Response(JSON.stringify(body), { status })) as unknown as typeof fetch;
    expect(await enterpriseSession(reply(200, { authenticated: true, name: 'Per' }))).toEqual({ authenticated: true, name: 'Per' });
    expect(await enterpriseSession(reply(401, { authenticated: false }))).toEqual({ authenticated: false });
    expect(await enterpriseSession(reply(200, { authenticated: 'yes' }))).toEqual({ authenticated: false, name: undefined });
    await expect(enterpriseSession(reply(503, {}))).rejects.toThrow();
  });
  it('explains every refusal with a recovery step', () => {
    for (const code of ['enterprise-login-required', 'enterprise-okta-unavailable', 'enterprise-owner-mismatch', 'enterprise-revoked'] as const) {
      expect(isEnterpriseDenial(code)).toBe(true);
      expect(denialMessage(code).error.length).toBeGreaterThan(20);
    }
    expect(isEnterpriseDenial('enterprise-other')).toBe(false);
    for (const code of ['denied', 'expired', 'rejected', 'configuration', 'revoked', 'unavailable', 'anything']) expect(loginErrorMessage(code)).toMatch(/\./);
  });
});
