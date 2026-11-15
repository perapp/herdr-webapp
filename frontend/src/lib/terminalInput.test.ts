import { describe, expect, it, vi } from 'vitest';
import { terminalInput } from './terminalInput';
import { TerminalSubmission } from './terminalSubmission';
import { createRequire } from 'node:module';

// Browser xterm's parser and input core can run without opening a DOM renderer.
vi.stubGlobal('self', globalThis);
const { Terminal } = await import('@xterm/xterm');
const require = createRequire(import.meta.url);

describe('xterm source contract and background-output regression', () => {
  it('pins the private adapter contract to the audited xterm version', () => {
    expect(require('@xterm/xterm/package.json').version).toBe('5.5.0');
    expect(() => terminalInput({} as InstanceType<typeof Terminal>, () => {})).toThrow('Unsupported terminal input source API');
  });
  it('keeps a partial draft unsent through completion paints and real parser queries', async () => {
    const term = new Terminal({ cols: 80, rows: 24 });
    const submission = new TerminalSubmission();
    const frames: { data: string; source: string }[] = [];
    let draft = '', submits = 0;
    const sub = terminalInput(term, (data, source) => {
      frames.push({ data, source });
      if (source !== 'input' || !submission.allow(data)) return;
      for (const char of data) { if (char === '\r' || char === '\n') submits++; else draft += char; }
    });
    const core = (term as unknown as { _core: { coreService: { triggerDataEvent(data: string, user?: boolean): void } } })._core.coreService;
    const write = (data: string) => new Promise<void>(resolve => term.write(data, resolve));
    try {
      // Delayed mobile/IME input retains its source even outside the DOM turn.
      await Promise.resolve();
      core.triggerDataEvent('DRAFT_NOT_SENT', true);
      await write('\x1b[?2026h\r\x1b[2Ktool completed\r\n\x1b[?2026l\x1b[6n\x1b[c');
      expect(draft).toBe('DRAFT_NOT_SENT');
      expect(submits).toBe(0);
      expect(frames.filter(frame => frame.source === 'input')).toEqual([{ data: 'DRAFT_NOT_SENT', source: 'input' }]);
      expect(frames.some(frame => frame.source === 'automatic' && frame.data.endsWith('R'))).toBe(true);
      // A stale textarea flush cannot silently submit even if xterm labels it
      // user input. Nor can an asynchronous IME completion append a newline.
      core.triggerDataEvent('DRAFT_NOT_SENT\r', true);
      expect(draft).toBe('DRAFT_NOT_SENT'); expect(submits).toBe(0);
      // Real Enter is never globally filtered.
      submission.markKey({ isTrusted: true, key: 'Enter' });
      core.triggerDataEvent('\r', true);
      expect(submits).toBe(1);
    } finally { sub.dispose(); term.dispose(); }
  });
  it('does not promote parser output during a nested user-input callback', () => {
    const term = new Terminal();
    const frames: string[] = [];
    const core = (term as unknown as { _core: { coreService: { triggerDataEvent(data: string, user?: boolean): void } } })._core.coreService;
    const sub = terminalInput(term, (data, source) => {
      frames.push(`${source}:${data}`);
      if (data === 'x') core.triggerDataEvent('\x1b[1;1R');
    });
    core.triggerDataEvent('x', true);
    expect(frames).toEqual(['input:x', 'automatic:\x1b[1;1R']);
    sub.dispose(); term.dispose();
  });
});
