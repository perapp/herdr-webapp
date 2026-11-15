import { describe, expect, it } from 'vitest';
import { applyModifiers, noModifiers } from './modifiers';

describe('latched modifiers', () => {
  const ctrl = { ctrl: true, alt: false }, alt = { ctrl: false, alt: true }, both = { ctrl: true, alt: true };
  it('leaves input alone when nothing is latched', () => {
    expect(applyModifiers('r', noModifiers())).toBe('r');
    expect(applyModifiers('\x1b[A', noModifiers())).toBe('\x1b[A');
  });
  it('encodes Ctrl as C0 controls regardless of letter case', () => {
    expect(applyModifiers('r', ctrl)).toBe('\x12');
    expect(applyModifiers('R', ctrl)).toBe('\x12');
    expect(applyModifiers('d', ctrl)).toBe('\x04');
    expect(applyModifiers('z', ctrl)).toBe('\x1a');
    expect(applyModifiers('[', ctrl)).toBe('\x1b');
    expect(applyModifiers(' ', ctrl)).toBe('\0');
    expect(applyModifiers('?', ctrl)).toBe('\x7f');
    expect(applyModifiers('5', ctrl)).toBe('5');
  });
  it('encodes Alt as an escape prefix and combines with Ctrl', () => {
    expect(applyModifiers('b', alt)).toBe('\x1bb');
    expect(applyModifiers('\x7f', alt)).toBe('\x1b\x7f');
    expect(applyModifiers('a', both)).toBe('\x1b\x01');
  });
  it('uses xterm modifier parameters for cursor and paging keys', () => {
    expect(applyModifiers('\x1b[D', alt)).toBe('\x1b[1;3D');
    expect(applyModifiers('\x1b[C', ctrl)).toBe('\x1b[1;5C');
    expect(applyModifiers('\x1b[A', both)).toBe('\x1b[1;7A');
    expect(applyModifiers('\x1b[5~', ctrl)).toBe('\x1b[5;5~');
  });
  it('never reinterprets multi-character text', () => {
    expect(applyModifiers('rm -rf', ctrl)).toBe('rm -rf');
    expect(applyModifiers('\x1b[200~c\x1b[201~', ctrl)).toBe('\x1b[200~c\x1b[201~');
    expect(applyModifiers('😀', alt)).toBe('\x1b😀');
  });
});
