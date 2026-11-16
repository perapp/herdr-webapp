import { describe, expect, it, vi } from 'vitest';
import { defaultTerminalPrefs, parseTerminalPrefs, reportGrid, watchGrid } from './terminalPrefs';

describe('terminal preferences', () => {
  it('defaults every switch to on', () => {
    expect(parseTerminalPrefs(null)).toEqual({ autoKeys: true, holdGrid: true, gpu: true });
    expect(parseTerminalPrefs(undefined)).toEqual(defaultTerminalPrefs);
  });
  it('reads stored switches and ignores unknown fields', () => {
    expect(parseTerminalPrefs('{"gpu":false,"future":1}')).toEqual({ autoKeys: true, holdGrid: true, gpu: false });
    expect(parseTerminalPrefs({ autoKeys: false, holdGrid: false, gpu: false })).toEqual({ autoKeys: false, holdGrid: false, gpu: false });
  });
  it('falls back per switch on malformed values', () => {
    expect(parseTerminalPrefs('not json')).toEqual(defaultTerminalPrefs);
    expect(parseTerminalPrefs('[false]')).toEqual(defaultTerminalPrefs);
    expect(parseTerminalPrefs('{"gpu":"no","holdGrid":0,"autoKeys":false}')).toEqual({ autoKeys: false, holdGrid: true, gpu: true });
  });
  it('never returns the shared defaults object', () => {
    const prefs = parseTerminalPrefs(null); prefs.gpu = false;
    expect(defaultTerminalPrefs.gpu).toBe(true);
  });
});
describe('reported terminal grid', () => {
  it('publishes changes only and replays the latest to new watchers', () => {
    const seen = vi.fn();
    const stop = watchGrid(seen);
    expect(seen).toHaveBeenLastCalledWith(undefined);
    reportGrid({ cols: 44, rows: 29 }); reportGrid({ cols: 44, rows: 29 });
    expect(seen).toHaveBeenCalledTimes(2);
    expect(seen).toHaveBeenLastCalledWith({ cols: 44, rows: 29 });
    const late = vi.fn(); const stopLate = watchGrid(late);
    expect(late).toHaveBeenCalledWith({ cols: 44, rows: 29 });
    stop(); stopLate(); reportGrid(undefined);
    expect(seen).toHaveBeenCalledTimes(2);
  });
});
