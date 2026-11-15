/** Latched accessory modifiers for keyboards without Ctrl/Alt. */
export interface Modifiers { ctrl: boolean; alt: boolean }
export const noModifiers = (): Modifiers => ({ ctrl: false, alt: false });

/** xterm modifier parameter: 1 + Alt(2) + Ctrl(4). */
function parameter(m: Modifiers): number { return 1 + (m.alt ? 2 : 0) + (m.ctrl ? 4 : 0); }
function control(char: string): string {
  const code = char.toUpperCase().charCodeAt(0);
  if (char === ' ') return '\0';
  if (char === '?') return '\x7f';
  // @ A-Z [ \ ] ^ _ : the C0 controls. Anything else has no Ctrl encoding.
  return code >= 0x40 && code <= 0x5f ? String.fromCharCode(code & 0x1f) : char;
}
/** Apply latched modifiers to exactly one key. Pasted or composed text of
 * several characters is never reinterpreted as a shortcut.
 */
export function applyModifiers(data: string, m: Modifiers): string {
  if (!m.ctrl && !m.alt) return data;
  if (data.startsWith('\x1b[')) {
    const body = data.slice(2);
    if (/^[A-DFH]$/.test(body)) return `\x1b[1;${parameter(m)}${body}`;
    if (/^\d+~$/.test(body)) return `\x1b[${body.slice(0, -1)};${parameter(m)}~`;
  }
  if ([...data].length !== 1) return data;
  const key = m.ctrl ? control(data) : data;
  return m.alt ? `\x1b${key}` : key;
}
