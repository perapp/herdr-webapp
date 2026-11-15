/** Browser terminals are regular native-style clients, including after reconnect. */
export function terminalUrl(address: string, cols: number, rows: number, href: string, theme: 'light' | 'dark' = 'light'): URL {
  const url = new URL('/api/terminal', href);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.search = new URLSearchParams({ terminal: address, cols: String(cols), rows: String(rows), observe: 'false', resumed: 'false', theme }).toString();
  return url;
}
/** Ctrl+J is LF, deliberately not Enter/CR. Shown first in the accessory's second row. */
export const ctrlJKey = { label: 'Ctrl J', name: 'Control J', data: '\n' };
/** `repeat` keys auto-repeat while held, like a hardware keyboard. The latched
 * Ctrl/Alt modifiers render after the first two; paging keys scroll off first
 * on narrow phones, where touch scrolling already covers scrollback.
 */
export const touchKeys: readonly { label: string; name: string; data: string; repeat?: boolean }[] = [
  { label: 'Esc', name: 'Escape', data: '\x1b' },
  { label: 'Tab', name: 'Tab', data: '\t' },
  { label: 'Ctrl C', name: 'Interrupt command', data: '\x03' },
  { label: '←', name: 'Arrow left', data: '\x1b[D', repeat: true },
  { label: '↓', name: 'Arrow down', data: '\x1b[B', repeat: true },
  { label: '↑', name: 'Arrow up', data: '\x1b[A', repeat: true },
  { label: '→', name: 'Arrow right', data: '\x1b[C', repeat: true },
  { label: 'PgUp', name: 'Page up', data: '\x1b[5~' },
  { label: 'PgDn', name: 'Page down', data: '\x1b[6~' },
];
