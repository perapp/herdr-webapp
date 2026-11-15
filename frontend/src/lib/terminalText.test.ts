import { describe, expect, it } from 'vitest';
import { screenText } from './terminalText';

function buffer(rows: [string, boolean?][], viewportY = 0) {
  return { viewportY, getLine: (y: number) => rows[y] && { isWrapped: !!rows[y][1], translateToString: (trim?: boolean) => trim ? rows[y][0].trimEnd() : rows[y][0] } };
}
describe('visible screen text', () => {
  it('copies only the viewport and drops trailing blank rows', () => {
    const b = buffer([['old   '], ['$ ls  '], ['a  b  '], ['      '], ['      ']], 1);
    expect(screenText(b, 4)).toBe('$ ls\na  b');
  });
  it('rejoins soft-wrapped rows without losing interior spaces', () => {
    const b = buffer([['echo a '], ['long   ', true], ['next   ']]);
    expect(screenText(b, 3)).toBe('echo a long\nnext');
  });
  it('keeps interior blank lines and tolerates a short buffer', () => {
    expect(screenText(buffer([['a'], [''], ['b']]), 10)).toBe('a\n\nb');
    expect(screenText(buffer([]), 5)).toBe('');
  });
});
