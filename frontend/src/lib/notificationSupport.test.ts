import { describe, expect, it } from 'vitest';
import { applicationServerKey, notificationLimitation } from './notificationSupport';
import type { NotificationEnvironment } from './notificationSupport';
const supported: NotificationEnvironment = { secure: true, notification: true, serviceWorker: true, push: true, standalone: false, userAgent: 'Chrome Android', platform: 'Linux arm', maxTouchPoints: 5 };
describe('notification capability explanations', () => {
  it('does not require installation on Android', () => { expect(notificationLimitation(supported)).toBeUndefined(); });
  it('explains Apple Home Screen requirement including desktop-style iPad identification', () => {
    expect(notificationLimitation({ ...supported, userAgent: 'Safari', platform: 'MacIntel' })).toContain('Home Screen');
    expect(notificationLimitation({ ...supported, userAgent: 'iPhone' })).toContain('16.4');
    expect(notificationLimitation({ ...supported, userAgent: 'iPhone', standalone: true })).toBeUndefined();
  });
  it('explains unavailable and insecure capabilities without guessing a cause', () => {
    expect(notificationLimitation({ ...supported, secure: false })).toContain('HTTPS');
    expect(notificationLimitation({ ...supported, push: false })).toContain('unavailable');
  });
  it('rejects malformed VAPID keys', () => {
    expect(() => applicationServerKey('secret?')).toThrow();
    expect(() => applicationServerKey('YWJj')).toThrow();
    const bytes = new Uint8Array(65); bytes[0] = 4;
    expect(applicationServerKey(btoa(String.fromCharCode(...bytes)).replace(/=+$/, '').replace(/\+/g, '-').replace(/\//g, '_')).byteLength).toBe(65);
  });
});
