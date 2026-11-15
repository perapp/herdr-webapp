/** Touch gestures as Svelte actions. Decisions are pure so they can be tested
 * without a DOM; the actions only wire pointer events to them.
 */
export const LONG_PRESS_MS = 500;

/** A deliberate, mostly horizontal flick: -1 previous (rightwards), 1 next (leftwards). */
export function swipeDirection(dx: number, dy: number, ms: number): -1 | 0 | 1 {
  if (ms > 700 || Math.abs(dx) < 48 || Math.abs(dx) < 2 * Math.abs(dy)) return 0;
  return dx < 0 ? 1 : -1;
}

export function swipe(node: HTMLElement, onswipe: (direction: -1 | 1) => void): { update(next: typeof onswipe): void; destroy(): void } {
  let start: { id: number; x: number; y: number; time: number } | undefined;
  const down = (event: PointerEvent) => {
    start = event.isTrusted && event.isPrimary && event.pointerType !== 'mouse' ? { id: event.pointerId, x: event.clientX, y: event.clientY, time: event.timeStamp } : undefined;
  };
  const up = (event: PointerEvent) => {
    const from = start; start = undefined;
    if (!from || from.id !== event.pointerId) return;
    const direction = swipeDirection(event.clientX - from.x, event.clientY - from.y, event.timeStamp - from.time);
    if (direction) onswipe(direction);
  };
  const cancel = () => { start = undefined; };
  node.addEventListener('pointerdown', down);
  node.addEventListener('pointerup', up);
  node.addEventListener('pointercancel', cancel);
  return { update(next) { onswipe = next; }, destroy() {
    node.removeEventListener('pointerdown', down); node.removeEventListener('pointerup', up); node.removeEventListener('pointercancel', cancel);
  } };
}

/** Long press on touch, plus the platform context-menu gesture (right click,
 * the keyboard Menu key, Android long press). The click that ends a long press
 * is swallowed so the row is not also activated.
 */
export function longPress(node: HTMLElement, onpress: () => void): { update(next: typeof onpress): void; destroy(): void } {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let x = 0, y = 0, fired = 0;
  const stop = () => { clearTimeout(timer); timer = undefined; };
  const fire = (time: number) => {
    stop();
    // Android reports the same hold as both our timer and a contextmenu event.
    if (time - fired < 800) return;
    fired = time; onpress();
  };
  const down = (event: PointerEvent) => {
    stop();
    if (!event.isTrusted || event.pointerType === 'mouse' || !event.isPrimary) return;
    x = event.clientX; y = event.clientY;
    timer = setTimeout(() => fire(performance.now()), LONG_PRESS_MS);
  };
  const move = (event: PointerEvent) => { if (timer && Math.hypot(event.clientX - x, event.clientY - y) > 10) stop(); };
  const menu = (event: MouseEvent) => { if (!event.isTrusted) return; event.preventDefault(); fire(performance.now()); };
  const click = (event: MouseEvent) => {
    if (performance.now() - fired > 800) return;
    fired = 0; event.preventDefault(); event.stopImmediatePropagation();
  };
  node.addEventListener('pointerdown', down);
  node.addEventListener('pointermove', move);
  node.addEventListener('pointerup', stop);
  node.addEventListener('pointercancel', stop);
  node.addEventListener('pointerleave', stop);
  node.addEventListener('contextmenu', menu);
  node.addEventListener('click', click, true);
  return { update(next) { onpress = next; }, destroy() {
    stop();
    node.removeEventListener('pointerdown', down); node.removeEventListener('pointermove', move);
    node.removeEventListener('pointerup', stop); node.removeEventListener('pointercancel', stop); node.removeEventListener('pointerleave', stop);
    node.removeEventListener('contextmenu', menu); node.removeEventListener('click', click, true);
  } };
}

/** The on-screen keyboard is inferred from the viewport losing height at an
 * unchanged width; a new width (rotation, split view) starts a new baseline.
 * Works whether the browser resizes the layout or only the visual viewport.
 */
export function keyboardDetector(threshold = 150): (width: number, height: number) => boolean {
  let baseWidth = 0, tallest = 0;
  return (width, height) => {
    if (Math.abs(width - baseWidth) > 1) { baseWidth = width; tallest = height; }
    tallest = Math.max(tallest, height);
    return tallest - height > threshold;
  };
}
