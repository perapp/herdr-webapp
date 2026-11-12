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
  const mark = vi.fn();
  const scrolling = touchScroll(host as unknown as HTMLElement, term as unknown as Terminal, { attached: () => attached, mark });
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
  return { term, reports, mark, scrolling, event, tick, frames,
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
  it('forwards bounded wheel steps with the trusted originating touch, without inertia', () => {
    const f = fixture();
    f.term.buffer.active.type = 'alternate'; f.term.modes.mouseTrackingMode = 'vt200';
    f.event('touchstart'); const move = f.event('touchmove', 120);
    expect(f.reports).toHaveLength(4);
    expect(f.mark).toHaveBeenCalledWith(move);
    expect(f.reports[0]).toMatchObject({ type: 'wheel', init: { deltaMode: 1, deltaY: 1, clientX: 50, clientY: 120 } });
    f.event('touchmove', -1000);
    expect(f.reports).toHaveLength(12);
    f.event('touchend'); f.tick();
    expect(f.reports).toHaveLength(12); expect(f.term.scrollLines).not.toHaveBeenCalled();
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
