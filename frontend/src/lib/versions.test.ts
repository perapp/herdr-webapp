import { afterEach, describe, expect, it, vi } from 'vitest';
import { commitDate, displayDate, fetchVersionMode, loadedBuild, modeBuild, normalizeBuild, timestampVersion, displayVersion, versionRows, versionUrl } from './versions';
import { emptyState, parseState } from './schema';
import { sourceBuild } from '../../buildInfo';
const a = 'a'.repeat(40), b = 'b'.repeat(40), iso = '2026-06-03T14:05:06+02:00';
afterEach(() => { vi.unstubAllGlobals(); });
function compiled() { vi.stubGlobal('__HERDR_APP_VERSION__', '0.0.8'); vi.stubGlobal('__HERDR_SOURCE_REVISION__', a); vi.stubGlobal('__HERDR_SOURCE_COMMIT_DATE__', iso); vi.stubGlobal('__HERDR_SOURCE_DIRTY__', false); }
describe('version provenance', () => {
  it('keeps loaded assets separate from newer server builds', () => {
    compiled();
    const served = { version: 'legacy', build: { version: 'new', revision: b, commitDate: iso, dirty: true } };
    const rows = versionRows(true, served, { version: 'connector', revision: a }, '', '0.1.19', true);
    expect(loadedBuild().revision).toBe(a); expect(rows[0].build.version).toBe('260603140506+0200-aaaaaaa');
    expect(rows[1].build.version).toBeNull(); expect(rows[2].build.revision).toBe(b);
    expect(rows[3].build).toEqual({ version: '0.1.19', revision: null, commitDate: null, dirty: null });
  });
  it('shows actual channel URLs only for browser-facing components, never connector/Core URLs', () => {
    compiled();
    const rows = versionRows(true, { publicOrigin: 'https://other.test', basePath: '/wrong/' }, null, '', '', true, 'https://relay.test/main/connections/computer?token=private#pair=private');
    expect(rows.map(row => row.url)).toEqual(['https://relay.test/main/', undefined, 'https://relay.test/main/', undefined]);
    const local = versionRows(false, {}, null, '', '', false, 'http://localhost:8001/launch#token=private');
    expect(local.map(row => row.url)).toEqual(['http://localhost:8001/', 'http://localhost:8001/', undefined, undefined]);
  });
  it('strips URL credentials, arbitrary paths and access material and rejects non-web URLs', () => {
    expect(versionUrl('https://user:password@relay.test:8443/main/connections/id?token=private#pair=private')).toBe('https://relay.test:8443/main/');
    expect(versionUrl('https://relay.test/private-link/path?token=private#token=private')).toBe('https://relay.test/');
    for (const href of ['not a URL', 'javascript:alert(1)', 'file:///main/', 'data:text/plain,private']) expect(versionUrl(href)).toBeUndefined();
  });
  it('supports legacy and null metadata without inferring a date or revision', () => {
    expect(modeBuild({ version: 'old', revision: a, buildRevision: a })).toEqual({ version: 'old', revision: null, commitDate: null, dirty: null });
    expect(normalizeBuild(null, 'old').version).toBe('old');
    expect(normalizeBuild(null).revision).toBeNull();
  });
  it('bounds untrusted fields and validates types, full SHA and actual timezone dates', () => {
    expect(normalizeBuild({ version: 'x'.repeat(161), revision: 'abc', commitDate: '2026-02-30T00:00:00Z', dirty: 'false' })).toEqual({ version: null, revision: null, commitDate: null, dirty: null });
    expect(normalizeBuild({ version: 'x\n', revision: a, dirty: false }).version).toBeNull();
    for (const value of ['2026-06-03', '2026-06-03T12:00:00', '2026-13-01T00:00:00Z', '2026-01-01T24:00:00Z', 'now']) expect(commitDate(value)).toBeNull();
    expect(commitDate(iso)).toBe(iso); expect(displayDate(iso)).toBe(new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'long' }).format(new Date(iso)));
  });
  it('local connection has no invented relay and snapshot uses only top-level Core version', () => {
    compiled();
    const snapshot = parseState({ ...emptyState(), connected: false, core: { identity: { version: 'not-core-version' } }, coreVersion: '0.1.19' });
    const rows = versionRows(false, { version: 'local' }, null, '', snapshot.coreVersion, snapshot.connected);
    expect(rows[1].build.version).toBe('local'); expect(rows[2].unused).toBe(true); expect(rows[2].build.version).toBeNull();
    expect(rows[3].detail).toContain('last observed'); expect(parseState({ ...emptyState(), connected: true, core: { version: 'wrong' } }).coreVersion).toBeUndefined();
    expect(parseState({ ...emptyState(), connected: true, coreVersion: 'x'.repeat(161) }).coreVersion).toBeUndefined();
  });
  it('queries only a matching HEAD for commit date and dirty state', () => {
    const git = vi.fn((args: string[]) => args[0] === 'rev-parse' ? a : args[0] === 'show' ? iso : ' M frontend/src/App.svelte');
    expect(sourceBuild({ HERDR_BUILD_REVISION: a }, git)).toMatchObject({ revision: a, commitDate: iso, dirty: true });
    expect(git).toHaveBeenCalledWith(['show', '-s', '--format=%cI', a]);
    git.mockClear(); expect(sourceBuild({ HERDR_BUILD_REVISION: b }, git)).toMatchObject({ revision: b, commitDate: null, dirty: null }); expect(git).toHaveBeenCalledTimes(1);
    expect(sourceBuild({ HERDR_BUILD_REVISION: b, HERDR_BUILD_COMMIT_DATE: iso }, git).commitDate).toBeNull();
    expect(sourceBuild({ HERDR_BUILD_REVISION: a, HERDR_BUILD_COMMIT_DATE: '2020-01-01T00:00:00Z' }, git).commitDate).toBe(iso);
    const absentGit = () => { throw new Error('No checkout'); };
    expect(sourceBuild({ HERDR_BUILD_REVISION: b, HERDR_BUILD_COMMIT_DATE: iso }, absentGit)).toMatchObject({ revision: b, commitDate: iso, dirty: null });
    expect(sourceBuild({ HERDR_BUILD_COMMIT_DATE: iso }, absentGit)).toMatchObject({ revision: '', commitDate: null, dirty: null });
    expect(normalizeBuild({ commitDate: iso, dirty: true })).toEqual({ version: null, revision: null, commitDate: null, dirty: null });
    expect(sourceBuild({ HERDR_BUILD_REVISION: b, HERDR_BUILD_COMMIT_DATE: 'yesterday' }, git).commitDate).toBeNull();
  });
  it('formats exact tags, untagged commits and unknown metadata honestly', () => {
    expect(timestampVersion(a, iso)).toBe('260603140506+0200-aaaaaaa');
    expect(timestampVersion(b, '2026-01-02T03:04:05-05:30')).toBe('260102030405-0530-bbbbbbb');
    expect(timestampVersion(b, '2026-01-02T03:04:05Z')).toBe('260102030405+0000-bbbbbbb');
    expect(displayVersion({ version: '0.0.8', revision: a, commitDate: iso })).toBe('260603140506+0200-aaaaaaa');
    expect(displayVersion({ version: '0.0.8', releaseVersion: '1.2.3', revision: a, commitDate: iso })).toBe('1.2.3');
    expect(displayVersion({ revision: a }, '0.0.8')).toBe('Unavailable');
    const tagged = normalizeBuild({ releaseVersion: '1.2.3', revision: a, commitDate: iso, dirty: false });
    expect(displayVersion(tagged)).toBe('1.2.3');
    expect(normalizeBuild(tagged)).toEqual(tagged);
    const git = (args: string[]) => args[0] === 'rev-parse' ? a : args[0] === 'show' ? iso : args[0] === 'tag' ? 'v1.2.3' : '';
    expect(sourceBuild({}, git)).toMatchObject({ revision: a, version: '1.2.3', releaseVersion: '1.2.3' });
    const dirty = sourceBuild({}, args => args[0] === 'status' ? ' M source.ts' : git(args));
    expect(dirty).toMatchObject({ dirty: true, releaseVersion: null, version: '260603140506+0200-aaaaaaa' });
    expect(displayVersion({ revision: a, commitDate: iso, releaseVersion: '1.2.3', dirty: true })).toBe('260603140506+0200-aaaaaaa');
    expect(sourceBuild({}, args => args[0] === 'tag' ? '' : git(args)).version).toBe('260603140506+0200-aaaaaaa');
    expect(sourceBuild({}, () => { throw new Error('No Git'); }).version).toBe('Unavailable');
  });
  it('fetches channel metadata directly using the native network and abort signal', async () => {
    vi.stubGlobal('location', new URL('https://example.test/main/'));
    const fetcher = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ mode: 'relay', build: { revision: b } }) }); vi.stubGlobal('fetch', fetcher);
    const controller = new AbortController(); await fetchVersionMode(controller.signal);
    expect(fetcher).toHaveBeenCalledWith('/main/api/mode', expect.objectContaining({ signal: controller.signal, cache: 'no-store', credentials: 'same-origin' }));
    fetcher.mockResolvedValue({ ok: false }); await expect(fetchVersionMode(controller.signal)).rejects.toThrow();
  });
});
