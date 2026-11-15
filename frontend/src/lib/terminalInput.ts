import type { Terminal } from '@xterm/xterm';

export type InputSource = 'input' | 'automatic';
interface CoreInput {
  triggerDataEvent(data: string, wasUserInput?: boolean): void;
  triggerBinaryEvent(data: string): void;
}

/** xterm 5.5's public onData erases the parser/user source flag. Preserve it at
 * the source, not by timing DOM events or guessing escape-sequence contents.
 * This deliberately bounded private adapter must be checked on xterm upgrades.
 * A missing hook fails closed rather than sending parser replies as keystrokes.
 */
export function terminalInput(term: Terminal, send: (data: string, source: InputSource, binary: boolean) => void): { dispose(): void } {
  const core = (term as unknown as { _core?: { coreService?: CoreInput } })._core?.coreService;
  if (!core || typeof core.triggerDataEvent !== 'function' || typeof core.triggerBinaryEvent !== 'function') {
    throw new Error('Unsupported terminal input source API. Reload after updating the app.');
  }
  const data = core.triggerDataEvent;
  let source: InputSource = 'automatic';
  core.triggerDataEvent = function (value, wasUserInput = false) {
    const previous = source;
    source = wasUserInput ? 'input' : 'automatic';
    try { return data.call(this, value, wasUserInput); }
    finally { source = previous; }
  };
  const dataSub = term.onData(value => send(value, source, false));
  // xterm uses onBinary only for intentional legacy mouse reports.
  const binarySub = term.onBinary(value => send(value, 'input', true));
  return { dispose() {
    dataSub.dispose(); binarySub.dispose();
    core.triggerDataEvent = data;
  } };
}
