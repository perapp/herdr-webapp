import { describe, expect, it, vi } from 'vitest';
import { agentSeenReporter, agentStatus, agentTurn, seenTurn } from './agentSeen';
import { drawnTerminals } from './navigation';
import { viewKey } from './schema';
import type { LayoutNode, TerminalInfo } from './schema';

const terminal = (address = 'beta/t1', turn = 2, seen: string | null = '1', state = 'idle'): TerminalInfo => ({ address, state: 'live', turn, status: { state }, labels: seen === null ? {} : { 'herdr.seen': seen } });

describe('native agent state from core turns and shared labels', () => {
  it('shows unseen finishes even if the word is idle, and seen done as idle', () => {
    expect(agentStatus(terminal())).toBe('done');
    expect(agentStatus(terminal('t', 2, '2', 'done'))).toBe('idle');
    expect(agentStatus(terminal('t', 2, '3', 'done'))).toBe('idle');
    for (const state of ['working', 'blocked', 'error']) expect(agentStatus(terminal('t', 2, '2', state))).toBe(state === 'error' ? 'blocked' : state);
    expect(agentStatus(undefined)).toBe('unreported');
  });
  it('does not invent turn zero or parse malformed/out-of-range counters', () => {
    for (const turn of [-1, 1.2, Number.MAX_SAFE_INTEGER + 1, '2', undefined]) expect(agentTurn({ turn })).toBeUndefined();
    for (const seen of ['-1', '1.2', ' 2', '18446744073709551616']) expect(seenTurn(terminal('t', 1, seen))).toBeUndefined();
    expect(seenTurn(terminal('t', 1, null))).toBeUndefined();
    expect(seenTurn(terminal('t', 1, '18446744073709551615'))).toBe(18446744073709551615n);
  });
});

describe('native share_seen exposure semantics', () => {
  it('writes real shared labels once per turn for every drawn live agent, not just focus', () => {
    const write = vi.fn().mockResolvedValue({}); const report = agentSeenReporter(write);
    const records = [terminal(), terminal('beta/t2', 4, null, 'working'), terminal('hidden')];
    report(records, ['beta/t1', 'beta/t2'], true); report(records, ['beta/t1', 'beta/t2'], true);
    expect(write.mock.calls).toEqual([['beta/t1', 2], ['beta/t2', 4]]);
    report([terminal('beta/t1', 3)], ['beta/t1'], true);
    expect(write).toHaveBeenLastCalledWith('beta/t1', 3);
  });
  it('respects foreground, live state, status and exact available turns; never lowers seen', () => {
    const write = vi.fn().mockResolvedValue({}); const report = agentSeenReporter(write);
    const records = [terminal(), terminal('seen', 2, '3'), { ...terminal('ended'), state: 'finished' }, { ...terminal('shell'), status: undefined }, { ...terminal('old'), turn: undefined }];
    report(records, records.map(t => t.address!), false); expect(write).not.toHaveBeenCalled();
    report(records, records.map(t => t.address!), true); expect(write.mock.calls).toEqual([['beta/t1', 2]]);
  });
  it('does not undo another client marking unread until looking again', () => {
    const write = vi.fn().mockResolvedValue({}); const report = agentSeenReporter(write);
    report([terminal('t', 2, '2')], ['t'], true);
    report([terminal('t', 2, '1')], ['t'], true); expect(write).not.toHaveBeenCalled();
    report([], [], false); report([terminal('t', 2, '1')], ['t'], true);
    expect(write.mock.calls).toEqual([['t', 2]]);
  });
  it('does not acknowledge the next finish on its status event before core raises turn', () => {
    const write = vi.fn().mockResolvedValue({}); const report = agentSeenReporter(write);
    report([terminal('t', 2, '2', 'working')], ['t'], true);
    report([terminal('t', 2, '2', 'done')], ['t'], true); expect(write).not.toHaveBeenCalled();
    report([terminal('t', 3, '2', 'done')], ['t'], true); expect(write).toHaveBeenCalledWith('t', 3);
  });
  it('releases failed writes for a later observation without changing displayed state', async () => {
    const write = vi.fn().mockRejectedValue(new Error('disconnected')); const report = agentSeenReporter(write);
    const t = terminal(); report([t], ['beta/t1'], true);
    await Promise.resolve(); await Promise.resolve();
    expect(agentStatus(t)).toBe('done'); report([t], ['beta/t1'], true); expect(write).toHaveBeenCalledTimes(2);
  });
});

describe('actual drawn terminal selection', () => {
  const leaf = (id: string): LayoutNode => ({ id, type: 'block', content: { terminal: id }, children: [] });
  it('excludes hidden and inactive stack children and qualifies remote addresses', () => {
    const tree: LayoutNode = { id: 'root', type: 'row', children: [leaf('a'), { id: 's', type: 'stack', active: 'b', children: [leaf('b'), leaf('c')] }, { ...leaf('d'), hidden: true }] };
    expect(drawnTerminals(tree, 'beta', {})).toEqual(['beta/a', 'beta/b']);
    expect(drawnTerminals(tree, null, { [viewKey(null, 's')]: 'c' })).toEqual(['a', 'c']);
    expect(drawnTerminals(leaf('c'), null, {})).toEqual(['c']); // phone/zoom root
    expect(drawnTerminals(undefined, null, {})).toEqual([]);
  });
});
