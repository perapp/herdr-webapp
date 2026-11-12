import { describe, expect, it } from 'vitest';
import { terminalUrl, touchKeys } from './terminal';

describe('native-style browser terminals', () => {
  it('attaches interactively on both initial connection and reconnect', () => {
    for (let reconnect = 0; reconnect < 3; reconnect++) {
      const url = terminalUrl('beta/t1', 43, 24, 'http://phone-visible-server:8001/');
      expect(url.origin).toBe('ws://phone-visible-server:8001');
      expect(url.searchParams.get('terminal')).toBe('beta/t1');
      expect(url.searchParams.get('observe')).toBe('false');
      expect(url.searchParams.get('resumed')).toBe('false');
      expect(url.searchParams.get('cols')).toBe('43');
    }
  });
  it('uses secure websockets behind an HTTPS origin', () => {
    expect(terminalUrl('t1', 80, 24, 'https://herdr.example/').protocol).toBe('wss:');
  });
  it('provides phone keyboard shortcuts without ownership controls', () => {
    expect(touchKeys.find(k => k.label === 'Ctrl C')?.data).toBe('\x03');
    expect(touchKeys.find(k => k.label === 'Tab')?.data).toBe('\t');
    expect(touchKeys.find(k => k.name === 'Page up')?.data).toBe('\x1b[5~');
    expect(touchKeys.find(k => k.name === 'Page down')?.data).toBe('\x1b[6~');
    expect(touchKeys.filter(k => k.name.startsWith('Arrow'))).toHaveLength(4);
  });
});
