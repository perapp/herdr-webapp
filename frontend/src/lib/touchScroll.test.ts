import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Terminal } from '@xterm/xterm';
import { ScrollRows, touchScroll } from './touchScroll';

function fixture() {
  const listeners = new Map<string, (event: never) => void>();
  let clock = 0, attached = true, reduced = false, id = 0;
  const frames = new Map<number, FrameRequestCallback>();
  const reports: unknown[] = [];
  const screen = { getBoundingClientRect: () => ({ height: 400 }), dispatchEvent: (event: unknown) => reports.push(event) };
  const host = {
    clientHeight: 400, querySelector: () => screen,
    addEventListener: (name: string, fn: (event: never) => void) => listeners.set(name, fn),
    removeEventListener: (name: string) => listeners.delete(name),
  };
  const term = {
    rows: 20, buffer: { active: { type: 'normal', viewportY: 100 } },
    modes: { mouseTrackingMode: 'none' }, hasSelection: () => false,
    scrollLines: vi.fn((n: number) => { term.buffer.active.viewportY = Math.max(0, Math.min(200, term.buffer.active.viewportY + n)); }),
  };
  vi.stubGlobal('document', { hidden: false, addEventListener: vi.fn(), removeEventListener: vi.fn() });
  vi.stubGlobal('window', { addEventListener: vi.fn(), removeEventListener: vi.fn() });
  vi.stubGlobal('performance', { now: () => clock });
  vi.stubGlobal('requestAnimationFrame', (fn: FrameRequestCallback) => { frames.set(++id, fn); return id; });
  vi.stubGlobal('cancelAnimationFrame', (n: number) => frames.delete(n));
  vi.stubGlobal('matchMedia', () => ({ matches: reduced }));
  vi.stubGlobal('WheelEvent', class { static DOM_DELTA_LINE = 1; constructor(public type: string, public init: unknown) {} });
  const mark = vi.fn(), selected = vi.fn();
  const scrolling = touchScroll(host as unknown as HTMLElement, term as unknown as Terminal, { attached: () => attached, mark, selected });
  function event(name: string, y = 200, extra = {}) {
    const e = { isTrusted: true, touches: [{ clientX: 50, clientY: y }], preventDefault: vi.fn(), stopPropagation: vi.fn(), stopImmediatePropagation: vi.fn(), ...extra };
    listeners.get(name)?.(e as never);
    return e;
  }
  function tick(ms = 16) {
    clock += ms;
    const queued = [...frames.values()]; frames.clear();
    queued.forEach(fn => fn(clock));
  }
  return { term, reports, mark, selected, screen, scrolling, event, tick, frames,
    disconnect: () => { attached = false; scrolling.reset(); },
    reduced: () => { reduced = true; },
  };
}
afterEach(() => vi.unstubAllGlobals());

describe('touch scrolling', () => {
  it('accumulates fractional rows without dropping small finger movements', () => {
    const rows = new ScrollRows();
    expect(rows.take(9, 20)).toBe(0);
    expect(rows.take(12, 20)).toBe(1);
    expect(rows.take(-22, 20)).toBe(-1);
    rows.clear();
    expect(rows.take(1, 20)).toBe(0);
  });
  it('batches local scrollback by animation frame, then adds local momentum', () => {
    const f = fixture();
    f.event('touchstart'); f.event('touchmove', 160); f.event('touchmove', 120);
    expect(f.term.scrollLines).not.toHaveBeenCalled();
    f.tick();
    expect(f.term.buffer.active.viewportY).toBe(104);
    f.event('touchend', 120); f.tick();
    expect(f.term.buffer.active.viewportY).toBeGreaterThan(104);
    expect(f.reports).toEqual([]); expect(f.mark).not.toHaveBeenCalled();
  });
  it('paces wheel reports per frame from the trusted originating touch', () => {
    const f = fixture();
    f.term.buffer.active.type = 'alternate'; f.term.modes.mouseTrackingMode = 'vt200';
    f.event('touchstart'); const move = f.event('touchmove', 120);
    // Nothing is sent synchronously from the touch handler: no bursts.
    expect(f.reports).toHaveLength(0);
    f.tick(); expect(f.reports).toHaveLength(3);
    f.tick(); expect(f.reports).toHaveLength(4);
    expect(f.mark).toHaveBeenCalledWith(move);
    expect(f.reports[0]).toMatchObject({ type: 'wheel', init: { deltaMode: 1, deltaY: 1, clientX: 50, clientY: 120 } });
    f.tick(); expect(f.reports).toHaveLength(4);
    expect(f.term.scrollLines).not.toHaveBeenCalled();
  });
  it('coasts briefly after a flick, bounds the backlog and stops on the next touch', () => {
    const f = fixture();
    f.term.buffer.active.type = 'alternate'; f.term.modes.mouseTrackingMode = 'vt200';
    f.event('touchstart'); f.event('touchmove', 160); f.tick(8); f.event('touchmove', -2000);
    f.event('touchend', -2000);
    for (let i = 0; i < 6; i++) f.tick();
    // A huge jump owes at most the bounded backlog, three steps per frame.
    expect(f.reports.length).toBeLessThanOrEqual(3 * 7);
    const coasted = f.reports.length;
    expect(coasted).toBeGreaterThan(12);
    for (let i = 0; i < 400; i++) f.tick();
    const total = f.reports.length;
    expect(total).toBeGreaterThan(coasted); expect(total).toBeLessThan(400);
    expect(f.frames.size).toBe(0);
    // Catching: a new touch ends the coast immediately.
    const g = fixture();
    g.term.buffer.active.type = 'alternate'; g.term.modes.mouseTrackingMode = 'vt200';
    g.event('touchstart'); g.event('touchmove', 160); g.tick(8); g.event('touchmove', 0); g.event('touchend', 0); g.tick();
    const before = g.reports.length;
    g.event('touchstart'); g.tick(); g.tick();
    expect(g.reports).toHaveLength(before);
  });
  it('sends no coasting wheel reports under reduced motion or after the mode changes', () => {
    const f = fixture(); f.reduced();
    f.term.buffer.active.type = 'alternate'; f.term.modes.mouseTrackingMode = 'vt200';
    f.event('touchstart'); f.event('touchmove', 120); f.event('touchend', 120);
    for (let i = 0; i < 20; i++) f.tick();
    expect(f.reports).toHaveLength(4);
    const g = fixture();
    g.term.buffer.active.type = 'alternate'; g.term.modes.mouseTrackingMode = 'vt200';
    g.event('touchstart'); g.event('touchmove', 0); g.event('touchend', 0);
    g.term.modes.mouseTrackingMode = 'none'; g.tick(); g.tick();
    expect(g.reports).toEqual([]); expect(g.frames.size).toBe(0);
  });
  it('selects the word under a long press and extends it while dragging', () => {
    vi.useFakeTimers();
    try {
      const f = fixture();
      const text = 'echo hello  world';
      const select = vi.fn();
      Object.assign(f.term, { cols: 20, select });
      Object.assign(f.term.buffer.active, { getLine: () => ({ getCell: (col: number) => ({ getChars: () => text[col] ?? '' }) }) });
      Object.assign(f.screen, { getBoundingClientRect: () => ({ left: 0, top: 0, width: 200, height: 400 }) });
      // Cells are 10 px wide and 20 px tall; the viewport starts at buffer row 100.
      f.event('touchstart', 45, { touches: [{ clientX: 72, clientY: 45 }] });
      vi.advanceTimersByTime(600);
      expect(select).toHaveBeenLastCalledWith(5, 102, 5); // "hello"
      const move = f.event('touchmove', 65, { touches: [{ clientX: 31, clientY: 65 }] });
      expect(move.preventDefault).toHaveBeenCalled();
      expect(select).toHaveBeenLastCalledWith(5, 102, 20 + 3 - 5 + 1);
      f.event('touchmove', 25, { touches: [{ clientX: 12, clientY: 25 }] });
      expect(select).toHaveBeenLastCalledWith(1, 101, 20 + 9 - 1 + 1);
      const end = f.event('touchend');
      expect(end.preventDefault).toHaveBeenCalled();
      expect(f.selected).toHaveBeenCalledTimes(1);
      expect(f.term.scrollLines).not.toHaveBeenCalled(); expect(f.reports).toEqual([]);
      // Moving before the hold completes is a scroll, never a selection.
      select.mockClear();
      f.event('touchstart'); f.event('touchmove', 160); vi.advanceTimersByTime(600); f.event('touchend');
      expect(select).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
  });
  it('leaves taps alone but suppresses the click after a drag', () => {
    const f = fixture();
    f.event('touchstart');
    const small = f.event('touchmove', 198);
    expect(small.stopPropagation).toHaveBeenCalled();
    expect(small.preventDefault).not.toHaveBeenCalled();
    f.event('touchend');
    expect(f.event('click').preventDefault).not.toHaveBeenCalled();
    f.event('touchstart'); f.event('touchmove', 160); f.event('touchend');
    expect(f.event('click').stopImmediatePropagation).toHaveBeenCalled();
    f.event('touchstart'); f.event('touchend');
    expect(f.event('click').preventDefault).not.toHaveBeenCalled();
  });
  it('never guesses arrow input in alternate-screen apps without mouse reporting', () => {
    const f = fixture(); f.term.buffer.active.type = 'alternate';
    f.event('touchstart'); f.event('touchmove', 100); f.event('touchend'); f.tick();
    expect(f.reports).toEqual([]); expect(f.mark).not.toHaveBeenCalled();
  });
  it('cancels pending momentum on disconnect and on mode changes', () => {
    const f = fixture();
    f.event('touchstart'); f.event('touchmove', 100); f.event('touchend');
    f.disconnect(); f.tick();
    expect(f.term.scrollLines).not.toHaveBeenCalled(); expect(f.frames.size).toBe(0);
    const g = fixture();
    g.event('touchstart'); g.event('touchmove', 100); g.event('touchend');
    g.term.buffer.active.type = 'alternate'; g.tick();
    expect(g.term.scrollLines).not.toHaveBeenCalled(); expect(g.reports).toEqual([]);
  });
  it('ignores untrusted gestures and preserves pinch gestures', () => {
    const f = fixture();
    f.event('touchstart', 200, { isTrusted: false }); f.event('touchmove', 100); f.tick();
    expect(f.term.scrollLines).not.toHaveBeenCalled();
    f.event('touchstart');
    const pinch = f.event('touchmove', 100, { touches: [{ clientX: 50, clientY: 100 }, { clientX: 80, clientY: 100 }] });
    expect(pinch.preventDefault).not.toHaveBeenCalled(); f.tick();
    expect(f.term.scrollLines).not.toHaveBeenCalled();
  });
  it('honours reduced motion while retaining the final drag delta and cleans up', () => {
    const f = fixture(); f.reduced();
    f.event('touchstart'); f.event('touchmove', 100); f.event('touchend'); f.tick();
    expect(f.term.buffer.active.viewportY).toBe(105); expect(f.frames.size).toBe(0);
    f.scrolling.dispose(); f.event('touchstart'); f.event('touchmove', 0); f.tick();
    expect(f.term.buffer.active.viewportY).toBe(105);
  });
});
