/** Terminal text and OSC 8 targets are untrusted program output. Only absolute
 * web URLs may leave the client; never delegate to a remote shell/browser.
 */
export function webLink(value: string): string | undefined {
  if (!/^https?:\/\//i.test(value) || [...value].some(char => char <= ' ' || char === '\x7f')) return;
  try {
    const url = new URL(value);
    if ((url.protocol === 'http:' || url.protocol === 'https:') && url.hostname && !url.username && !url.password) return url.href;
  } catch { /* Not a URL. */ }
}

export function openTerminalLink(event: MouseEvent, value: string): void {
  const href = webLink(value);
  if (!event.isTrusted || !href) return;
  // The xterm linkifier supplies the trusted click, rather than a DOM anchor.
  // Activate a real anchor synchronously inside that gesture: browser handles
  // navigation locally, with no app popup, opener access or referrer leakage.
  event.preventDefault();
  event.stopPropagation();
  const anchor = document.createElement('a');
  anchor.href = href;
  anchor.target = '_blank';
  anchor.rel = 'noopener noreferrer';
  anchor.referrerPolicy = 'no-referrer';
  anchor.click();
}
