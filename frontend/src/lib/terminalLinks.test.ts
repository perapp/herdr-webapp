import { describe, expect, it, vi } from 'vitest';
import { openTerminalLink, webLink } from './terminalLinks';

describe('client terminal web links', () => {
  it('allows absolute HTTP(S) only, rejecting controls, credentials and unsafe schemes', () => {
    expect(webLink('https://example.com/a?q=b#c')).toBe('https://example.com/a?q=b#c');
    expect(webLink('HTTP://localhost:9001')).toBe('http://localhost:9001/');
    for (const value of ['javascript:alert(1)', 'file:///etc/passwd', 'data:text/html,x', '//example.com', 'https://user:pass@example.com', 'https://example.com\n', 'https://', ' https://example.com']) expect(webLink(value)).toBeUndefined();
  });
  it('activates a no-opener no-referrer client anchor only in the trusted gesture', () => {
    const anchor = { click: vi.fn(), href: '', target: '', rel: '', referrerPolicy: '' };
    const createElement = vi.fn(() => anchor);
    vi.stubGlobal('document', { createElement });
    const event = { isTrusted: true, preventDefault: vi.fn(), stopPropagation: vi.fn() } as unknown as MouseEvent;
    try {
      openTerminalLink({ ...event, isTrusted: false }, 'https://example.com');
      openTerminalLink(event, 'javascript:alert(1)');
      expect(createElement).not.toHaveBeenCalled();
      openTerminalLink(event, 'https://example.com');
      expect(anchor).toMatchObject({ href: 'https://example.com/', target: '_blank', rel: 'noopener noreferrer', referrerPolicy: 'no-referrer' });
      expect(anchor.click).toHaveBeenCalledOnce();
    } finally { vi.unstubAllGlobals(); }
  });
});
