import { afterEach, describe, expect, it, vi } from 'vitest';
import { AccessController, parseAccessStatus } from './access';
import { captureTransportRequest, setApplicationTransport } from './transport';
import type { ApplicationTransport } from './transport';

const status = { supported: true, canRotate: true, scope: 'computer', tokenLifetime: 'untilRotated', sessionLifetime: 'untilRevoked' };
const json = (value: unknown, code = 200) => new Response(JSON.stringify(value), { status: code });
afterEach(() => setApplicationTransport(undefined));

describe('persistent access controls', () => {
  it('validates capabilities without accepting credentials or malformed lifetimes', () => {
    expect(parseAccessStatus(status)).toEqual(status);
    for (const value of [null, {}, { ...status, supported: false }, { ...status, scope: 'all' }, { ...status, tokenLifetime: 'fiveMinutes' }]) {
      expect(() => parseAccessStatus(value)).toThrow();
    }
  });
  it('requires confirmation, allows cancel, and submits one exact empty mutation', async () => {
    const send = vi.fn().mockResolvedValueOnce(json(status)).mockResolvedValueOnce(json({ rotated: true }));
    const release = vi.fn(); const busy = vi.fn(() => release);
    const changed = vi.fn(); const controller = new AccessController(send, changed, busy);
    await controller.load(); await controller.rotate(); expect(send).toHaveBeenCalledTimes(1);
    controller.ask(); controller.cancel(); await controller.rotate(); expect(send).toHaveBeenCalledTimes(1);
    controller.ask(); const rotating = controller.rotate(); await controller.rotate(); await rotating;
    expect(send).toHaveBeenCalledTimes(2);
    expect(send.mock.calls[1][0]).toBe('/api/access/rotate');
    expect(send.mock.calls[1][1]).toMatchObject({ method: 'POST', body: '{}', credentials: 'same-origin', cache: 'no-store', redirect: 'error' });
    expect(busy).toHaveBeenCalledTimes(1); expect(release).toHaveBeenCalledTimes(1);
    expect(controller.state.message).toContain('This browser stays signed in');
    expect(controller.state.pending).toBe(false);
  });
  it('does not replay an ambiguous rotation or render a response body containing credentials', async () => {
    const send = vi.fn().mockResolvedValueOnce(json(status)).mockResolvedValueOnce(new Response('synthetic-sensitive-value', { status: 503 }));
    const controller = new AccessController(send, vi.fn());
    await controller.load(); controller.ask(); await controller.rotate();
    expect(controller.state.uncertain).toBe(true);
    expect(controller.state.issue).not.toContain('synthetic-sensitive-value');
    controller.ask(); await controller.rotate(); expect(send).toHaveBeenCalledTimes(2);
  });
  it('treats unauthorized and unsupported controls explicitly', async () => {
    for (const code of [400, 404, 405, 501]) {
      const send = vi.fn().mockResolvedValueOnce(json({}, code));
      const controller = new AccessController(send, vi.fn()); await controller.load();
      expect(controller.state.unavailable).toBe(true); controller.ask(); expect(controller.state.confirming).toBe(false);
    }
    const reject = vi.fn().mockResolvedValueOnce(json(status)).mockResolvedValueOnce(json({}, 403));
    const rejected = new AccessController(reject, vi.fn()); await rejected.load(); rejected.ask(); await rejected.rotate();
    expect(rejected.state.uncertain).toBe(false); expect(rejected.state.issue).toContain('rejected');
  });
  it('binds rotation to the selected authenticated computer, not a later transport', async () => {
    const first = vi.fn().mockResolvedValueOnce(json(status)).mockResolvedValueOnce(json({ rotated: true }));
    const second = vi.fn();
    const facade = (request: ApplicationTransport['request']): ApplicationTransport => ({ request, events: vi.fn(), socket: vi.fn() });
    setApplicationTransport(facade(first)); const controller = new AccessController(captureTransportRequest(), vi.fn());
    await controller.load(); controller.ask(); setApplicationTransport(facade(second)); await controller.rotate();
    expect(first).toHaveBeenCalledTimes(2); expect(second).not.toHaveBeenCalled();
  });
  it('does not mutate or publish after the settings scope is disposed', async () => {
    const send = vi.fn().mockResolvedValue(json(status)); const changed = vi.fn();
    const controller = new AccessController(send, changed); await controller.load(); controller.ask();
    controller.stop(); changed.mockClear(); await controller.rotate(); await controller.load();
    expect(send).toHaveBeenCalledTimes(1); expect(changed).not.toHaveBeenCalled();
  });
});
