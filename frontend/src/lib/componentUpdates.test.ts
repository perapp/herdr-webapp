import { afterEach, describe, expect, it, vi } from 'vitest';
import { canUpdateComponent, clearUpdateNotice, componentBusy, componentCandidate, noteUpdate, parseComponentUpdates, settleUpdateNotice, updateNotice, updateScope, watchUpdateNotices } from './componentUpdates';
import type { ComponentUpdate } from './componentUpdates';
const core: ComponentUpdate = { state: 'available', currentVersion: '1.2.3', availableVersion: '1.2.4', identity: 'stable:1.2.4', channel: 'stable', managed: 'connector launcher', instruction: null, error: null, canCheck: true, canInstall: true };
const snapshot = () => parseComponentUpdates({ connector: { ...core }, core: { ...core } });
afterEach(() => { clearUpdateNotice('/one'); clearUpdateNotice('/two'); vi.unstubAllGlobals(); });
describe('component update boundary', () => {
  it('validates complete snapshots, capability gates, bounded metadata and opaque identities', () => {
    expect(snapshot().core.identity).toBe('stable:1.2.4');
    for (const bad of [null, [], { ...core, canInstall: true, identity: null }, { ...core, availableVersion: null }, { ...core, currentVersion: 'x\n' }, { ...core, error: 'x'.repeat(4097) }, { ...core, managed: 1 }, { ...core, state: 'done' }, { ...core, availableRevision: 'short' }]) {
      expect(() => parseComponentUpdates({ connector: core, core: bad })).toThrow();
    }
    expect(parseComponentUpdates({ connector: core, core: { ...core, canInstall: false, identity: null, availableVersion: null, instruction: 'Use your package manager.\nThen reconnect.' } }).core.instruction).toContain('package manager');
  });
  it('only offers intentional managed updates from the available state', () => {
    expect(canUpdateComponent(snapshot().core)).toBe(true);
    for (const change of [{ canInstall: false }, { state: 'unchecked' }, { state: 'checking' }, { state: 'updating' }, { state: 'reconnecting' }, { state: 'error' }]) {
      expect(canUpdateComponent({ ...snapshot().core, ...change } as ComponentUpdate)).toBe(false);
    }
    expect(componentBusy({ ...snapshot().core, state: 'updating' })).toBe(true);
    expect(componentCandidate({ ...snapshot().connector, channel: 'main', availableRevision: 'a'.repeat(40) })).toBe('Main build aaaaaaa');
  });
  it('isolates progress by channel/computer, never stores a replayable request and requires observed success', () => {
    vi.stubGlobal('location', new URL('https://herdr.test/main/connections/abc'));
    expect(updateScope()).toBe('/main/abc');
    vi.stubGlobal('location', new URL('https://herdr.test/connections/abc'));
    expect(updateScope()).toBe('/abc');
    const listener = vi.fn(), stop = watchUpdateNotices(listener);
    noteUpdate('/one', 'core', snapshot().core);
    expect(updateNotice('/two')).toBeUndefined();
    expect(updateNotice('/one')).not.toHaveProperty('identity');
    settleUpdateNotice('/one', { ...snapshot().core, state: 'current' });
    expect(updateNotice('/one')?.state).toBe('pending');
    settleUpdateNotice('/one', { ...snapshot().core, state: 'current', currentVersion: '1.2.4' });
    expect(updateNotice('/one')?.state).toBe('done');
    expect(listener).toHaveBeenCalledTimes(2); stop();
  });
  it('keeps failure explicit and does not turn a failed operation into success', () => {
    noteUpdate('/two', 'connector', snapshot().connector);
    settleUpdateNotice('/two', { ...snapshot().connector, state: 'error', error: 'Download failed. Retry check.' });
    expect(updateNotice('/two')).toMatchObject({ state: 'error', message: 'Download failed. Retry check.' });
    settleUpdateNotice('/two', { ...snapshot().connector, currentVersion: '1.2.4' });
    expect(updateNotice('/two')?.state).toBe('error');
  });
});
