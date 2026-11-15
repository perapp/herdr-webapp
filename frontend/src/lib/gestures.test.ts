import { describe, expect, it } from 'vitest';
import { keyboardDetector, swipeDirection } from './gestures';

describe('swipe decision', () => {
  it('maps a leftward flick to next and a rightward flick to previous', () => {
    expect(swipeDirection(-80, 5, 200)).toBe(1);
    expect(swipeDirection(80, -5, 200)).toBe(-1);
  });
  it('ignores short, slow or diagonal movement', () => {
    expect(swipeDirection(-30, 0, 100)).toBe(0);
    expect(swipeDirection(-80, 0, 900)).toBe(0);
    expect(swipeDirection(-80, 50, 200)).toBe(0);
  });
});
describe('keyboard detection', () => {
  it('reports a keyboard when height drops at the same width', () => {
    const open = keyboardDetector();
    expect(open(390, 760)).toBe(false);
    expect(open(390, 430)).toBe(true);
    expect(open(390, 760)).toBe(false);
  });
  it('ignores browser chrome changes and learns a taller baseline', () => {
    const open = keyboardDetector();
    expect(open(390, 700)).toBe(false);
    expect(open(390, 760)).toBe(false);
    expect(open(390, 700)).toBe(false);
    expect(open(390, 560)).toBe(true);
  });
  it('starts over after rotation instead of comparing orientations', () => {
    const open = keyboardDetector();
    open(390, 760);
    expect(open(844, 340)).toBe(false);
    expect(open(844, 150)).toBe(true);
  });
});
