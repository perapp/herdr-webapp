import { describe, expect, it } from 'vitest';
import { canInstallUpdate, detectUpdatedVersion, parseUpdateStatus, releaseURL, updateBusy, validVersion } from './updates';
import type { UpdateStatus } from './updates';
const status: UpdateStatus = {
  current_version: '0.0.0', available_version: '0.0.1', phase: 'available', message: 'Ready',
  can_install: true, release_url: releaseURL('0.0.1'), checked_at: 1700000000, source: 'release',
};
describe('update status boundary', () => {
  it('accepts release and development status without manufacturing install support', () => {
    expect(parseUpdateStatus(status)).toEqual(status);
    const source = parseUpdateStatus({ ...status, source: 'development', can_install: false });
    expect(canInstallUpdate(source)).toBe(false);
    expect(canInstallUpdate({ ...source, can_install: true })).toBe(false);
  });
  it.each(['260603140506+0200-abcdef0', '260102030405-0530-abcdef0', 'Unavailable'])('accepts development display identity %s without treating it as SemVer', current_version => {
    const value = parseUpdateStatus({ ...status, current_version, source: 'development', can_install: false });
    expect(value.current_version).toBe(current_version);
    expect(validVersion(current_version)).toBe(false);
    expect(canInstallUpdate(value)).toBe(false);
    expect(() => parseUpdateStatus({ ...status, available_version: current_version })).toThrow();
  });
  it.each(['0.0.1', '1.2.3-rc.1', '1.2.3+build.01', '1.2.3-0.a-1'])('accepts SemVer %s', value => expect(validVersion(value)).toBe(true));
  it.each(['v1.2.3', '01.2.3', '1.2', '1.2.3-01', '1.2.3-', '1.2.3\n', '1.2.3/<script>', '1'.repeat(129)])('rejects invalid version %s', value => expect(validVersion(value)).toBe(false));
  it.each([
    null, [], {}, { ...status, current_version: 1 }, { ...status, available_version: 'evil' },
    { ...status, phase: 'installed' }, { ...status, can_install: 'true' },
    { ...status, source: 'github' }, { ...status, message: 'x'.repeat(2049) },
    { ...status, message: '\u0000' }, { ...status, checked_at: -1 },
    { ...status, checked_at: Infinity }, { ...status, checked_at: 0.5 },
  ])('rejects malformed status %#', value => expect(() => parseUpdateStatus(value)).toThrow());
  it.each([
    'http://github.com/perapp/herdr-webapp/releases/tag/v0.0.1',
    'https://github.com.evil.test/perapp/herdr-webapp/releases/tag/v0.0.1',
    'https://github.com/perapp/herdr-webapp/releases/tag/v0.0.2',
    'https://github.com/perapp/herdr-webapp/releases/tag/v0.0.1?token=secret',
    'javascript:alert(1)',
  ])('rejects untrusted release links %s', release_url => {
    expect(() => parseUpdateStatus({ ...status, release_url })).toThrow();
  });
  it('allows absent release links but not links without an available version', () => {
    expect(parseUpdateStatus({ ...status, release_url: null, available_version: null }).available_version).toBeNull();
    expect(() => parseUpdateStatus({ ...status, available_version: null })).toThrow();
  });
  it('keeps messages plain text, not markup', () => {
    expect(parseUpdateStatus({ ...status, message: '<b>Sign in with gh on the server</b>' }).message).toBe('<b>Sign in with gh on the server</b>');
  });
});
describe('install and reload transitions', () => {
  it('installs only an available supported release outside active work', () => {
    expect(canInstallUpdate(status)).toBe(true);
    for (const phase of ['checking', 'downloading', 'restarting'] as const) {
      expect(updateBusy({ ...status, phase })).toBe(true);
      expect(canInstallUpdate({ ...status, phase })).toBe(false);
    }
    expect(canInstallUpdate(null)).toBe(false);
    expect(canInstallUpdate({ ...status, available_version: null })).toBe(false);
    expect(canInstallUpdate({ ...status, available_version: status.current_version })).toBe(false);
    expect(canInstallUpdate({ ...status, can_install: false })).toBe(false);
  });
  it('does not reload for ordinary checks or other clients’ installs', () => {
    expect(detectUpdatedVersion({ fromVersion: null, reloaded: false }, status)).toBe(false);
  });
  it('waits for a changed running version after user intent, then reloads only once', () => {
    const intent = { fromVersion: '0.0.0', reloaded: false };
    expect(detectUpdatedVersion(intent, status)).toBe(false);
    expect(detectUpdatedVersion(intent, { ...status, phase: 'restarting' })).toBe(false);
    expect(detectUpdatedVersion(intent, { ...status, current_version: '0.0.1', available_version: null, phase: 'idle' })).toBe(true);
    expect(detectUpdatedVersion(intent, { ...status, current_version: '0.0.1' })).toBe(false);
  });
});
