import { describe, expect, it } from 'vitest';
import { TerminalSubmission } from './terminalSubmission';

describe('terminal submit intent', () => {
  it('requires explicit Enter, not a text/composition flush, focus or redraw', () => {
    const gate = new TerminalSubmission();
    expect(gate.allow('partial')).toBe(true);
    expect(gate.allow('partial\n')).toBe(false);
    gate.markKey({ isTrusted: true, key: 'Enter', isComposing: true });
    expect(gate.allow('\r')).toBe(false);
    gate.markKey({ isTrusted: false, key: 'Enter' });
    expect(gate.allow('\r')).toBe(false);
    gate.markKey({ isTrusted: true, key: 'Enter' });
    expect(gate.allow('\r')).toBe(true);
    expect(gate.allow('\r')).toBe(false);
  });
  it('preserves mobile line-break, accessory Enter, Alt-Enter and multiline paste', () => {
    const gate = new TerminalSubmission();
    gate.markInput({ isTrusted: true, inputType: 'insertLineBreak' });
    expect(gate.allow('\r')).toBe(true);
    gate.markEnter({ isTrusted: true });
    expect(gate.allow('\r')).toBe(true);
    gate.markKey({ isTrusted: true, key: 'Enter' });
    expect(gate.allow('\x1b\r')).toBe(true);
    gate.markKey({ isTrusted: true, key: 'j', ctrlKey: true });
    expect(gate.allow('\n')).toBe(true);
    gate.markKey({ isTrusted: true, key: 'm', ctrlKey: true });
    expect(gate.allow('\r')).toBe(true);
    gate.markPaste({ isTrusted: true, text: 'a\nb' });
    expect(gate.allow('\x1b[200~a\rb\x1b[201~')).toBe(true);
    gate.markPaste({ isTrusted: true, text: 'a\nb' });
    expect(gate.allow('a\rb\r')).toBe(false);
  });
  it('allows one trusted accessory Control J, never an unsolicited or untrusted line feed', () => {
    const gate = new TerminalSubmission();
    expect(gate.allow('\n')).toBe(false);
    gate.markShortcut({ isTrusted: false }, '\n');
    expect(gate.allow('\n')).toBe(false);
    gate.markShortcut({ isTrusted: true }, '\n');
    expect(gate.allow('\n')).toBe(true);
    expect(gate.allow('\n')).toBe(false);
    gate.markShortcut({ isTrusted: true }, '\n');
    expect(gate.allow('unexpected flush\n')).toBe(false);
    gate.markShortcut({ isTrusted: true }, '\n'); gate.clear();
    expect(gate.allow('\n')).toBe(false);
    gate.markShortcut({ isTrusted: true }, '\n');
    gate.markShortcut({ isTrusted: true }, '\x03');
    expect(gate.allow('\n')).toBe(false);
  });
  it('revokes stale Enter on other keys, blur and disconnect', () => {
    const gate = new TerminalSubmission();
    gate.markEnter({ isTrusted: true });
    gate.markKey({ isTrusted: true, key: 'x' });
    expect(gate.allow('\r')).toBe(false);
    gate.markEnter({ isTrusted: true }); gate.clear();
    expect(gate.allow('\r')).toBe(false);
  });
});
