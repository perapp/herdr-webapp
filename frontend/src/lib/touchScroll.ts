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

/** Own single-finger scrolling, leaving taps, selection and pinch zoom alone.
 * Application wheel reports go through xterm's negotiated mouse protocol, never
 * handwritten escape sequences. Momentum is local scrollback only, not input.
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
  const stopFrame = () => { cancelAnimationFrame(frame); frame = 0; pending = 0; velocity = 0; rows.clear(); };
  const reset = () => { stopFrame(); tracking = dragging = false; };
  const scrollLocal = (pixels: number): boolean => {
    const count = rows.take(pixels, rowHeight());
    const before = term.buffer.active.viewportY;
    if (count) term.scrollLines(count);
    return !count || term.buffer.active.viewportY !== before;
  };
  const animate = (time: number) => {
    frame = 0;
    if (!options.attached() || document.hidden || currentMode() !== mode || !local()) { reset(); return; }
    const elapsed = Math.min(32, Math.max(0, time - lastTime));
    lastTime = time;
    const pixels = pending + (tracking ? 0 : velocity * elapsed);
    pending = 0;
    if (!scrollLocal(pixels)) velocity = 0;
    if (!tracking) velocity *= Math.exp(-elapsed / 220);
    if (pending || (!tracking && Math.abs(velocity) > 0.02)) frame = requestAnimationFrame(animate);
  };
  const start = (event: TouchEvent) => {
    reset();
    suppressClick = false;
    if (!event.isTrusted || event.touches.length !== 1 || !options.attached() || term.hasSelection()) return;
    const touch = event.touches[0]!;
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
    velocity = Math.max(-3, Math.min(3, delta / Math.max(8, now - sampleTime)));
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
