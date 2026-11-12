import { describe, expect, it } from 'vitest';
import { InputGate, binaryInput, inputChunks, retryDelay, validSize } from './input';

describe('intentional terminal input classification', () => {
  it('defaults DSR and parser-generated output to automatic', () => {
    expect(new InputGate().classify()).toBe('automatic');
  });
  it('marks only a trusted DOM event turn as intentional input', async () => {
    const gate = new InputGate();
    gate.mark({ isTrusted: false });
    expect(gate.classify()).toBe('automatic');
    gate.mark({ isTrusted: true });
    expect(gate.classify()).toBe('input');
    await Promise.resolve();
    expect(gate.classify()).toBe('automatic');
  });
  it('clears ownership intent on detach, including queued event cleanup', async () => {
    const gate = new InputGate();
    gate.mark({ isTrusted: true }); gate.clear();
    expect(gate.classify()).toBe('automatic');
    gate.mark({ isTrusted: true });
    await Promise.resolve();
    expect(gate.classify()).toBe('automatic');
  });
  it('keeps delayed phone/IME composition intentional without promoting VT replies', async () => {
    const gate = new InputGate();
    gate.markComposition({ isTrusted: true, data: 'å終' });
    await Promise.resolve();
    expect(gate.classify('\x1b[1;1R')).toBe('automatic');
    expect(gate.classify('å終')).toBe('input');
    expect(gate.classify('å終')).toBe('automatic');
  });
  it('rejects synthetic/control-sequence compositions and clears pending IME on disconnect', async () => {
    const gate = new InputGate();
    gate.markComposition({ isTrusted: false, data: 'fake' });
    expect(gate.classify('fake')).toBe('automatic');
    gate.markComposition({ isTrusted: true, data: '\x1b[6n' });
    expect(gate.classify('\x1b[6n')).toBe('automatic');
    gate.markComposition({ isTrusted: true, data: '終' });
    gate.clear();
    await Promise.resolve();
    expect(gate.classify('終')).toBe('automatic');
  });
  it('preserves phone input across capture/target microtask boundaries', async () => {
    const gate = new InputGate();
    gate.markInput({ isTrusted: true, data: 'phone text', inputType: 'insertText' });
    await Promise.resolve();
    expect(gate.classify('\x1b[1;1R')).toBe('automatic');
    expect(gate.classify('phone text')).toBe('input');
    gate.markInput({ isTrusted: true, data: null, inputType: 'deleteContentBackward' });
    await Promise.resolve();
    expect(gate.classify('\x7f')).toBe('input');
  });
  it('recognizes trusted bracketed/multiline paste but never synthetic input', async () => {
    const gate = new InputGate();
    gate.markPaste({ isTrusted: true, text: 'hello\nworld' });
    await Promise.resolve();
    expect(gate.classify('\x1b[6n')).toBe('automatic');
    expect(gate.classify('\x1b[200~hello\rworld\x1b[201~')).toBe('input');
    gate.markInput({ isTrusted: false, data: 'fake', inputType: 'insertText' });
    gate.markPaste({ isTrusted: false, text: 'fake' });
    expect(gate.classify('fake')).toBe('automatic');
  });
  it('chunks large pastes within WS limits without splitting unicode', () => {
    const data = 'x'.repeat(4095) + '🙂' + '\u0001'.repeat(20000) + '終'.repeat(20000);
    const chunks = [...inputChunks(data)];
    expect(chunks.join('')).toBe(data);
    for (const chunk of chunks) {
      expect(new TextEncoder().encode(chunk).length).toBeLessThanOrEqual(16384);
      expect(new TextEncoder().encode(JSON.stringify({ type: 'input', data: chunk })).length).toBeLessThan(65536);
      expect(chunk.endsWith('\ud83d')).toBe(false);
    }
  });
  it('preserves binary bytes and caps reconnect backoff', () => {
    expect([...binaryInput('\x00\x80\xff')]).toEqual([0, 128, 255]);
    expect(retryDelay(0)).toBe(1000);
    expect(retryDelay(100)).toBe(30000);
    expect(validSize(80, 24)).toBe(true);
    expect(validSize(0, 24)).toBe(false);
    expect(validSize(80, Infinity)).toBe(false);
  });
});
