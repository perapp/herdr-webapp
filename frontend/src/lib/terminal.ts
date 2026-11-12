/** Browser terminals are regular native-style clients, including after reconnect. */
export function terminalUrl(address: string, cols: number, rows: number, href: string, theme: 'light' | 'dark' = 'light'): URL {
  const url = new URL('/api/terminal', href);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.search = new URLSearchParams({ terminal: address, cols: String(cols), rows: String(rows), observe: 'false', resumed: 'false', theme }).toString();
  return url;
}
export const touchKeys = [
  { label: 'Esc', name: 'Escape', data: '\x1b' },
  { label: 'Tab', name: 'Tab', data: '\t' },
  { label: 'Ctrl C', name: 'Interrupt command', data: '\x03' },
  { label: '←', name: 'Arrow left', data: '\x1b[D' },
  { label: '↓', name: 'Arrow down', data: '\x1b[B' },
  { label: '↑', name: 'Arrow up', data: '\x1b[A' },
  { label: '→', name: 'Arrow right', data: '\x1b[C' },
];
