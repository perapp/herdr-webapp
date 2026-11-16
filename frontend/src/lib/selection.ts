import type { Terminal } from '@xterm/xterm';

/** Touch text selection for a terminal drawn on a canvas, where the browser
 * has no selection of its own. Pure geometry and ranges; the gestures live in
 * `touchScroll` (long press) and the pane (drag handles).
 */

/** A buffer cell: column, and row counted from the top of history. */
export interface Cell { col: number; row: number }
/** Both ends inclusive, `from` never after `to`. */
export interface CellRange { from: Cell; to: Cell }
export const beforeCell = (a: Cell, b: Cell) => a.row < b.row || (a.row === b.row && a.col < b.col);

type Grid = Pick<Terminal, 'cols' | 'rows'> & { buffer: { active: Pick<Terminal['buffer']['active'], 'viewportY'> } };
/** The cell under a client point, clamped to the visible grid. */
export function cellAtPoint(term: Grid, screen: Pick<Element, 'getBoundingClientRect'> | null | undefined, clientX: number, clientY: number): Cell | undefined {
  const box = screen?.getBoundingClientRect();
  if (!box || !box.width || !box.height) return;
  const clamp = (value: number, max: number) => Math.max(0, Math.min(max, Math.floor(value)));
  return { col: clamp((clientX - box.left) / (box.width / term.cols), term.cols - 1), row: clamp((clientY - box.top) / (box.height / term.rows), term.rows - 1) + term.buffer.active.viewportY };
}
export const selectCells = (term: Pick<Terminal, 'select' | 'cols'>, { from, to }: CellRange) => term.select(from.col, from.row, (to.row - from.row) * term.cols + to.col - from.col + 1);

/** The run of non-blank cells around `cell`; a blank cell selects itself. */
export function wordAt(cell: Cell, cols: number, filled: (col: number) => boolean): CellRange {
  let from = cell.col, to = cell.col;
  if (filled(cell.col)) { while (from > 0 && filled(from - 1)) from--; while (to < cols - 1 && filled(to + 1)) to++; }
  return { from: { col: from, row: cell.row }, to: { col: to, row: cell.row } };
}
/** Dragging on from a long press: the pressed word stays selected whichever way the finger goes. */
export function extendRange(anchor: CellRange, cell: Cell): CellRange {
  return { from: beforeCell(cell, anchor.from) ? cell : anchor.from, to: beforeCell(anchor.to, cell) ? cell : anchor.to };
}
/** Dragging one handle: the other end stays put, and passing it flips the range. */
export function dragRange(fixed: Cell, cell: Cell): CellRange {
  return beforeCell(cell, fixed) ? { from: cell, to: fixed } : { from: fixed, to: cell };
}

export interface HandleGeometry { left: number; top: number; cellWidth: number; cellHeight: number; rows: number; viewportY: number }
export type HandlePlace = { left: number; top: number } | undefined;
/** Where the two handles hang: under the start's left edge and the end's right
 * edge, in the coordinates `left`/`top` are given in. A handle whose row is
 * scrolled out of view has no place.
 */
export function handlePlaces(range: CellRange, g: HandleGeometry): { start: HandlePlace; end: HandlePlace } {
  const at = (cell: Cell, edge: number): HandlePlace => {
    const row = cell.row - g.viewportY;
    return row < 0 || row >= g.rows ? undefined : { left: g.left + (cell.col + edge) * g.cellWidth, top: g.top + (row + 1) * g.cellHeight };
  };
  return { start: at(range.from, 0), end: at(range.to, 1) };
}
