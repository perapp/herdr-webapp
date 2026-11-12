import { afterEach, describe, expect, it, vi } from 'vitest';
import { request } from './api';

afterEach(() => vi.unstubAllGlobals());
describe('API error diagnostics', () => {
  it('preserves a structured sign-in failure', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({error:{message:'Invalid access token'}}), {status:401})));
    await expect(request('/api/login', {token:'test'})).rejects.toMatchObject({message:'Invalid access token',status:401});
  });
  it('preserves an origin rejection from an older plain-text server', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('Origin does not match this server', {status:403})));
    await expect(request('/api/login', {token:'test'})).rejects.toMatchObject({message:'Origin does not match this server',status:403});
  });
  it('does not accept a malformed success response', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('not JSON', {status:200})));
    await expect(request('/api/session')).rejects.toMatchObject({message:'The server returned an unreadable response.'});
  });
});
