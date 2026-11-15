import type { Terminal } from '@xterm/xterm';

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

/** Own single-finger scrolling, leaving taps, selection and pinch zoom alone.
 * Application wheel reports go through xterm's negotiated mouse protocol, never
 * handwritten escape sequences. Momentum is local scrollback only, not input.
 *
 * Local scrollback moves by pixels, not rows: xterm scrolls whole rows and the
 * screen is translated by the remaining fraction, then settles on a row.
 */
export function touchScroll(host: HTMLElement, term: Terminal, options: {
  attached: () => boolean;
  mark: (event: TouchEvent) => void;
}): { reset: () => void; dispose: () => void } {
  const rows = new ScrollRows();
  let startX = 0, startY = 0, lastY = 0, lastTime = 0, sampleTime = 0;
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
  const stopFrame = () => { cancelAnimationFrame(frame); frame = 0; pending = 0; velocity = 0; rows.clear(); };
  const reset = () => { stopFrame(); tracking = dragging = false; fraction = 0; shift(0); };
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
    shift(fraction);
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
      if (term.buffer.active.viewportY !== before) { shift(fraction - 1); void screen()?.offsetHeight; }
    }
    fraction = 0;
    shift(0, 'transform 110ms ease-out');
  };
  const animate = (time: number) => {
    frame = 0;
    if (!options.attached() || document.hidden || currentMode() !== mode || !local()) { reset(); return; }
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
    reset();
    suppressClick = false;
    if (!event.isTrusted || event.touches.length !== 1 || !options.attached() || term.hasSelection()) return;
    const touch = event.touches[0]!;
    // A finger landing on a glide catches the page where it is.
    const style = screen()?.style; if (style) style.transition = '';
    startX = x = touch.clientX; startY = lastY = y = touch.clientY;
    sampleTime = lastTime = performance.now(); mode = currentMode(); tracking = true;
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
    x = touch.clientX; y = touch.clientY;
    if (!dragging) {
      if (Math.abs(y - startY) < 6) return;
      if (Math.abs(x - startX) > Math.abs(y - startY)) { reset(); return; }
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
    } else {
      lastTime = now;
      // No guessed arrow/page input: programs must opt into mouse reporting.
      if (term.modes.mouseTrackingMode === 'none') return;
      const count = Math.max(-8, Math.min(8, rows.take(delta, rowHeight())));
      const screen = host.querySelector('.xterm-screen');
      for (let i = 0; screen && i < Math.abs(count); i++) {
        options.mark(event);
        screen.dispatchEvent(new WheelEvent('wheel', {
          bubbles: true, cancelable: true, clientX: x, clientY: y,
          deltaY: Math.sign(count), deltaMode: WheelEvent.DOM_DELTA_LINE,
        }));
      }
    }
  };
  const end = (event: TouchEvent) => {
    if (!tracking) return;
    if (dragging) { event.preventDefault(); event.stopPropagation(); }
    tracking = false;
    if (dragging && local() && !matchMedia('(prefers-reduced-motion: reduce)').matches) {
      if (performance.now() - sampleTime > 100) velocity = 0;
      if (!frame) { lastTime = performance.now(); frame = requestAnimationFrame(animate); }
    } else {
      if (local() && pending) scrollLocal(pending);
      stopFrame();
      if (local()) { if (fraction >= 0.5 && term.buffer.active.viewportY < term.buffer.active.baseY) term.scrollLines(1); fraction = 0; shift(0); }
    }
    dragging = false;
  };
  const click = (event: MouseEvent) => {
    if (!suppressClick) return;
    suppressClick = false; event.preventDefault(); event.stopImmediatePropagation();
  };
  const cancel = () => reset();
  host.addEventListener('touchstart', start, { capture: true, passive: true });
  host.addEventListener('touchmove', move, { capture: true, passive: false });
  host.addEventListener('touchend', end, { capture: true, passive: false });
  host.addEventListener('touchcancel', cancel, true);
  host.addEventListener('click', click, true);
  document.addEventListener('visibilitychange', cancel);
  window.addEventListener('blur', cancel);
  return { reset, dispose: () => {
    reset();
    host.removeEventListener('touchstart', start, true);
    host.removeEventListener('touchmove', move, true);
    host.removeEventListener('touchend', end, true);
    host.removeEventListener('touchcancel', cancel, true);
    host.removeEventListener('click', click, true);
    document.removeEventListener('visibilitychange', cancel);
    window.removeEventListener('blur', cancel);
  } };
}
