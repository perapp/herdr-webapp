import type { Terminal } from '@xterm/xterm';
import { LONG_PRESS_MS } from './gestures';
import { cellAtPoint, extendRange, selectCells, wordAt } from './selection';
import type { CellRange } from './selection';

/** Fractional pixels survive across frames; xterm's public scroll API uses rows. */
export class ScrollRows {
  private remainder = 0;
  take(pixels: number, rowHeight: number): number {
    this.remainder += pixels / Math.max(1, rowHeight);
    const rows = Math.trunc(this.remainder);
    this.remainder -= rows;
    return rows;
  }
  clear(): void { this.remainder = 0; }
}

/** Fling feel of a web page: velocity decays over this time constant, so a
 * flick travels about `velocity × FLING_MS` pixels.
 */
export const FLING_MS = 650;
const MAX_VELOCITY = 8; // px/ms
/** Programs that scroll themselves redraw per wheel step over the connection:
 * a shorter coast, at most a few steps per frame, and a bounded backlog so the
 * program never keeps scrolling long after the finger has gone.
 */
export const WHEEL_FLING_MS = 420;
const WHEEL_STEPS_PER_FRAME = 3, WHEEL_BACKLOG = 12;

/** Own single-finger scrolling, leaving taps, selection and pinch zoom alone.
 * Application wheel reports go through xterm's negotiated mouse protocol, never
 * handwritten escape sequences. Wheel reports are paced one animation frame at
 * a time and coast briefly after a flick; a new touch stops them at once.
 *
 * Local scrollback moves by pixels, not rows: xterm scrolls whole rows and the
 * screen is translated by the remaining fraction, then settles on a row.
 */
export function touchScroll(host: HTMLElement, term: Terminal, options: {
  attached: () => boolean;
  mark: (event: TouchEvent) => void;
  /** A long-press selection was finished at this client position. */
  selected?: (at: { x: number; y: number }, range: CellRange) => void;
}): { reset: () => void; dispose: () => void } {
  let startY = 0, lastY = 0, lastTime = 0, sampleTime = 0;
  let x = 0, y = 0, velocity = 0, pending = 0, frame = 0;
  let tracking = false, dragging = false, suppressClick = false;
  let mode = '';
  const currentMode = () => `${term.buffer.active.type}:${term.modes.mouseTrackingMode}`;
  const local = () => term.buffer.active.type === 'normal' && term.modes.mouseTrackingMode === 'none';
  const rowHeight = () => (host.querySelector('.xterm-screen')?.getBoundingClientRect().height || host.clientHeight) / term.rows;
  // Rows the content is shifted up beyond xterm's row position, in [0, 1).
  let fraction = 0;
  const screen = () => host.querySelector<HTMLElement>('.xterm-screen');
  const shift = (rowsUp: number, transition = '') => {
    const style = screen()?.style;
    if (!style) return;
    style.transition = transition;
    style.transform = rowsUp ? `translate3d(0,${-rowsUp * rowHeight()}px,0)` : '';
  };
  const stopFrame = () => { cancelAnimationFrame(frame); frame = 0; pending = 0; velocity = 0; owed = 0; };
  // xterm redraws rows on its own animation frame. Moving the pixel offset
  // before that redraw lands shows the old rows at the new offset: a one-frame
  // jump back by a row at every row boundary. Apply both in the same frame.
  let awaiting = false, easing = false;
  const rendered = term.onRender?.(() => {
    if (!awaiting) return;
    awaiting = false; shift(fraction);
    if (easing) { easing = false; void screen()?.offsetHeight; fraction = 0; shift(0, 'transform 110ms ease-out'); }
  });
  // Long press selects the word under the finger; dragging on extends it.
  // Text is drawn on a canvas, so the browser has no selection of its own here.
  let press: ReturnType<typeof setTimeout> | undefined;
  let anchor: CellRange | undefined;
  let touchedAt = -Infinity;
  const cellAt = (clientX: number, clientY: number) => cellAtPoint(term, screen(), clientX, clientY);
  let range: CellRange | undefined;
  const selectRange = (next: CellRange) => { range = next; selectCells(term, next); };
  const beginSelection = () => {
    press = undefined;
    if (!tracking || dragging || !options.attached()) return;
    const cell = cellAt(x, y);
    if (!cell) return;
    const line = term.buffer.active.getLine(cell.row);
    anchor = wordAt(cell, term.cols, col => !!line?.getCell(col)?.getChars().trim());
    selectRange(anchor); suppressClick = true;
  };
  const halt = () => { stopFrame(); clearTimeout(press); press = undefined; anchor = undefined; tracking = dragging = false; };
  const reset = () => { halt(); fraction = 0; awaiting = easing = false; shift(0); };
  /** Returns false once an end of the scrollback is reached. */
  const scrollLocal = (pixels: number): boolean => {
    const total = fraction + pixels / Math.max(1, rowHeight());
    const count = Math.floor(total);
    const buffer = term.buffer.active;
    const before = buffer.viewportY;
    if (count) term.scrollLines(count);
    // Past the top, or at the newest line where nothing follows the last row.
    const edge = buffer.viewportY - before !== count || (pixels > 0 && buffer.viewportY >= buffer.baseY);
    fraction = edge ? 0 : total - count;
    if (rendered && buffer.viewportY !== before) awaiting = true;
    if (!awaiting) shift(fraction);
    return !edge;
  };
  /** Glide the remaining fraction to the nearest row instead of jumping. */
  const settle = () => {
    if (!fraction) return;
    const forward = fraction >= 0.5 && term.buffer.active.viewportY < term.buffer.active.baseY;
    if (forward) {
      // One row further, drawn where it already was, then eased into place.
      const before = term.buffer.active.viewportY;
      term.scrollLines(1);
      if (rendered && term.buffer.active.viewportY !== before) { fraction -= 1; awaiting = easing = true; return; }
    }
    if (awaiting) { easing = true; return; }
    fraction = 0;
    shift(0, 'transform 110ms ease-out');
  };
  // Wheel rows owed to a program that scrolls itself, and the trusted touch
  // that every report of this gesture, coasting included, originates from.
  let owed = 0;
  let origin: TouchEvent | undefined;
  const wheel = (time: number) => {
    const elapsed = Math.min(32, Math.max(0, time - lastTime));
    lastTime = time;
    if (!tracking) { owed += velocity * elapsed / Math.max(1, rowHeight()); velocity *= Math.exp(-elapsed / WHEEL_FLING_MS); }
    owed = Math.max(-WHEEL_BACKLOG, Math.min(WHEEL_BACKLOG, owed));
    const count = Math.max(-WHEEL_STEPS_PER_FRAME, Math.min(WHEEL_STEPS_PER_FRAME, Math.trunc(owed)));
    owed -= count;
    const target = screen();
    for (let i = 0; target && origin && i < Math.abs(count); i++) {
      options.mark(origin);
      target.dispatchEvent(new WheelEvent('wheel', {
        bubbles: true, cancelable: true, clientX: x, clientY: y,
        deltaY: Math.sign(count), deltaMode: WheelEvent.DOM_DELTA_LINE,
      }));
    }
    if (Math.abs(owed) >= 1 || (!tracking && Math.abs(velocity) > 0.02)) frame = requestAnimationFrame(animate);
  };
  const animate = (time: number) => {
    frame = 0;
    if (!options.attached() || document.hidden || currentMode() !== mode) { reset(); return; }
    if (!local()) {
      // No guessed arrow/page input: programs must opt into mouse reporting.
      if (term.modes.mouseTrackingMode === 'none') reset(); else wheel(time);
      return;
    }
    const elapsed = Math.min(32, Math.max(0, time - lastTime));
    lastTime = time;
    const pixels = pending + (tracking ? 0 : velocity * elapsed);
    pending = 0;
    if (!scrollLocal(pixels)) velocity = 0;
    if (!tracking) velocity *= Math.exp(-elapsed / FLING_MS);
    if (pending || (!tracking && Math.abs(velocity) > 0.02)) frame = requestAnimationFrame(animate);
    else if (!tracking) settle();
  };
  const start = (event: TouchEvent) => {
    // A finger landing on a glide catches the page exactly where it is: keep
    // the sub-row offset instead of snapping to a row under the finger.
    halt(); easing = false;
    const style = screen()?.style; if (style) style.transition = '';
    suppressClick = false;
    // A selection (a double tap selects a word) must not disable scrolling.
    if (!event.isTrusted || event.touches.length !== 1 || !options.attached()) { reset(); return; }
    const touch = event.touches[0]!;
    x = touch.clientX; startY = lastY = y = touch.clientY;
    sampleTime = lastTime = touchedAt = performance.now(); mode = currentMode(); tracking = true;
    press = setTimeout(beginSelection, LONG_PRESS_MS);
    // Do not let xterm also install its own scrolling state for this gesture.
    event.stopPropagation();
  };
  const move = (event: TouchEvent) => {
    if (!tracking) return;
    if (!event.isTrusted || event.touches.length !== 1 || !options.attached() || currentMode() !== mode) { reset(); return; }
    // Even sub-threshold movement must not reach xterm: we intercepted its
    // touchstart, so its built-in handler has no valid starting coordinate.
    event.stopPropagation();
    const touch = event.touches[0]!;
    if (press && Math.hypot(touch.clientX - x, touch.clientY - y) > 8) { clearTimeout(press); press = undefined; }
    x = touch.clientX; y = touch.clientY;
    if (anchor) {
      event.preventDefault();
      const cell = cellAt(x, y);
      if (cell) selectRange(extendRange(anchor, cell));
      return;
    }
    if (!dragging) {
      // Thumbs move in arcs. A sideways start is not a reason to ignore the
      // rest of the gesture: wait for vertical travel, however it began.
      if (Math.abs(y - startY) < 6) return;
      dragging = suppressClick = true;
    }
    event.preventDefault(); event.stopPropagation();
    const now = performance.now();
    const delta = lastY - y;
    // Smoothed, so one late touch sample neither kills nor doubles the fling.
    const sample = Math.max(-MAX_VELOCITY, Math.min(MAX_VELOCITY, delta / Math.max(8, now - sampleTime)));
    velocity = dragging && velocity ? 0.65 * sample + 0.35 * velocity : sample;
    lastY = y; sampleTime = now;
    if (local()) {
      pending += delta;
      if (!frame) { lastTime = now; frame = requestAnimationFrame(animate); }
    } else if (term.modes.mouseTrackingMode !== 'none') {
      owed += delta / Math.max(1, rowHeight()); origin = event;
      if (!frame) { lastTime = now; frame = requestAnimationFrame(animate); }
    }
  };
  const end = (event: TouchEvent) => {
    if (!tracking) return;
    clearTimeout(press); press = undefined;
    if (anchor) {
      // No compatibility mouse events: xterm would clear the selection.
      event.preventDefault(); event.stopPropagation();
      anchor = undefined; tracking = false;
      if (range) options.selected?.({ x, y }, range);
      return;
    }
    if (dragging) { event.preventDefault(); event.stopPropagation(); }
    tracking = false;
    const coasting = local() || term.modes.mouseTrackingMode !== 'none';
    if (dragging && coasting && !matchMedia('(prefers-reduced-motion: reduce)').matches) {
      if (performance.now() - sampleTime > 100) velocity = 0;
      if (!frame) { lastTime = performance.now(); frame = requestAnimationFrame(animate); }
    } else if (dragging && coasting && !local()) {
      // Reduced motion: deliver what the finger already asked for, no coasting.
      velocity = 0;
      if (Math.abs(owed) >= 1 && !frame) { lastTime = performance.now(); frame = requestAnimationFrame(animate); }
    } else {
      if (local() && pending) scrollLocal(pending);
      stopFrame();
      if (local()) settle();
    }
    dragging = false;
  };
  const click = (event: MouseEvent) => {
    if (!suppressClick) return;
    suppressClick = false; event.preventDefault(); event.stopImmediatePropagation();
  };
  const cancel = () => reset();
  // Android reports a long press as a context-menu request as well.
  const menu = (event: MouseEvent) => { if (performance.now() - touchedAt < 1500) event.preventDefault(); };
  host.addEventListener('touchstart', start, { capture: true, passive: true });
  host.addEventListener('touchmove', move, { capture: true, passive: false });
  host.addEventListener('touchend', end, { capture: true, passive: false });
  host.addEventListener('touchcancel', cancel, true);
  host.addEventListener('click', click, true);
  host.addEventListener('contextmenu', menu);
  document.addEventListener('visibilitychange', cancel);
  window.addEventListener('blur', cancel);
  return { reset, dispose: () => {
    reset(); rendered?.dispose();
    host.removeEventListener('touchstart', start, true);
    host.removeEventListener('touchmove', move, true);
    host.removeEventListener('touchend', end, true);
    host.removeEventListener('touchcancel', cancel, true);
    host.removeEventListener('click', click, true);
    host.removeEventListener('contextmenu', menu);
    document.removeEventListener('visibilitychange', cancel);
    window.removeEventListener('blur', cancel);
  } };
}
