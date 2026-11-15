/** Copy with the async API where the page is a secure context, and the legacy
 * command elsewhere (the pinned LAN URL is plain HTTP). Reading has no such
 * fallback: callers hide paste when `canReadClipboard()` is false.
 */
export async function writeClipboard(text: string): Promise<void> {
  if (typeof navigator.clipboard?.writeText === 'function') {
    try { await navigator.clipboard.writeText(text); return; } catch { /* Denied or unfocused: try the legacy path. */ }
  }
  const focused = document.activeElement instanceof HTMLElement ? document.activeElement : undefined;
  const area = document.createElement('textarea');
  area.value = text; area.readOnly = true; area.setAttribute('aria-hidden', 'true');
  area.style.cssText = 'position:fixed;top:0;left:0;width:1px;height:1px;opacity:0';
  document.body.append(area);
  try {
    area.select(); area.setSelectionRange(0, text.length);
    if (!document.execCommand('copy')) throw new Error('Copy command rejected');
  } finally {
    area.remove();
    // Do not leave the terminal without the focus (and keyboard) it had.
    focused?.focus({ preventScroll: true });
  }
}
export function canReadClipboard(): boolean { return typeof navigator !== 'undefined' && typeof navigator.clipboard?.readText === 'function'; }
