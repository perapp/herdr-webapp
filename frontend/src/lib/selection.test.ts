import { describe, expect, it, vi } from 'vitest';
import { beforeCell, cellAtPoint, dragRange, extendRange, handlePlaces, selectCells, wordAt } from './selection';

const grid = (viewportY = 100) => ({ cols: 20, rows: 20, buffer: { active: { viewportY } } });
const screen = { getBoundingClientRect: () => ({ left: 10, top: 50, width: 200, height: 400 }) as DOMRect };

describe('touch selection geometry', () => {
  it('orders cells by row, then column', () => {
    expect(beforeCell({ col: 9, row: 1 }, { col: 0, row: 2 })).toBe(true);
    expect(beforeCell({ col: 3, row: 2 }, { col: 4, row: 2 })).toBe(true);
    expect(beforeCell({ col: 4, row: 2 }, { col: 4, row: 2 })).toBe(false);
  });
  it('maps a point to a buffer cell and clamps outside the grid', () => {
    // Cells are 10 px wide and 20 px tall, offset by the screen's position.
    expect(cellAtPoint(grid(), screen, 10 + 72, 50 + 45)).toEqual({ col: 7, row: 102 });
    expect(cellAtPoint(grid(), screen, -500, -500)).toEqual({ col: 0, row: 100 });
    expect(cellAtPoint(grid(), screen, 5000, 5000)).toEqual({ col: 19, row: 119 });
    expect(cellAtPoint(grid(), undefined, 0, 0)).toBeUndefined();
    expect(cellAtPoint(grid(), { getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0 }) as DOMRect }, 0, 0)).toBeUndefined();
  });
  it('selects an inclusive range across rows', () => {
    const select = vi.fn();
    selectCells({ select, cols: 20 }, { from: { col: 5, row: 102 }, to: { col: 3, row: 103 } });
    expect(select).toHaveBeenCalledWith(5, 102, 19);
    selectCells({ select, cols: 20 }, { from: { col: 4, row: 7 }, to: { col: 4, row: 7 } });
    expect(select).toHaveBeenLastCalledWith(4, 7, 1);
  });
  it('finds the word under a cell', () => {
    const text = 'echo hello  world';
    const filled = (col: number) => !!(text[col] ?? '').trim();
    expect(wordAt({ col: 7, row: 3 }, 20, filled)).toEqual({ from: { col: 5, row: 3 }, to: { col: 9, row: 3 } });
    expect(wordAt({ col: 0, row: 3 }, 20, filled)).toEqual({ from: { col: 0, row: 3 }, to: { col: 3, row: 3 } });
    // A blank cell selects only itself, and a word stops at the last column.
    expect(wordAt({ col: 10, row: 3 }, 20, filled)).toEqual({ from: { col: 10, row: 3 }, to: { col: 10, row: 3 } });
    expect(wordAt({ col: 2, row: 0 }, 4, () => true)).toEqual({ from: { col: 0, row: 0 }, to: { col: 3, row: 0 } });
  });
  it('keeps the pressed word while a long press drags on', () => {
    const anchor = { from: { col: 5, row: 102 }, to: { col: 9, row: 102 } };
    expect(extendRange(anchor, { col: 3, row: 103 })).toEqual({ from: anchor.from, to: { col: 3, row: 103 } });
    expect(extendRange(anchor, { col: 1, row: 101 })).toEqual({ from: { col: 1, row: 101 }, to: anchor.to });
    expect(extendRange(anchor, { col: 7, row: 102 })).toEqual(anchor);
  });
  it('flips the range when a handle passes the other end', () => {
    const fixed = { col: 9, row: 102 };
    expect(dragRange(fixed, { col: 2, row: 102 })).toEqual({ from: { col: 2, row: 102 }, to: fixed });
    expect(dragRange(fixed, { col: 2, row: 104 })).toEqual({ from: fixed, to: { col: 2, row: 104 } });
    expect(dragRange(fixed, fixed)).toEqual({ from: fixed, to: fixed });
  });
  it('places handles under the ends and hides one that scrolled away', () => {
    const g = { left: 4, top: 8, cellWidth: 10, cellHeight: 20, rows: 20, viewportY: 100 };
    const range = { from: { col: 5, row: 102 }, to: { col: 9, row: 103 } };
    expect(handlePlaces(range, g)).toEqual({ start: { left: 54, top: 68 }, end: { left: 104, top: 88 } });
    expect(handlePlaces(range, { ...g, viewportY: 103 })).toEqual({ start: undefined, end: { left: 104, top: 28 } });
    expect(handlePlaces(range, { ...g, viewportY: 60 })).toEqual({ start: undefined, end: undefined });
  });
});
