import { record, reportedStatus, terminalAddress } from './schema';
import type { TerminalInfo } from './schema';

/** Core counters must be represented exactly before writing shared labels. */
export function agentTurn(terminal: TerminalInfo | undefined): number | undefined {
  return Number.isSafeInteger(terminal?.turn) && (terminal!.turn as number) >= 0 ? terminal!.turn as number : undefined;
}
export function seenTurn(terminal: TerminalInfo | undefined): bigint | undefined {
  const value = record(terminal?.labels) ? terminal.labels['herdr.seen'] : undefined;
  if (typeof value !== 'string' || !/^\+?\d+$/.test(value)) return;
  const parsed = BigInt(value);
  return parsed <= 18446744073709551615n ? parsed : undefined;
}
/** Shared labels take precedence; missing labels retain legacy reported completion. */
export function agentStatus(terminal: TerminalInfo | undefined): string {
  const word = reportedStatus(terminal);
  if (word === 'error') return 'blocked';
  if (word === 'working' || word === 'blocked' || word === 'unreported') return word;
  const turn = agentTurn(terminal), seen = seenTurn(terminal);
  return turn !== undefined && seen !== undefined ? BigInt(turn) > seen ? 'done' : 'idle' : word;
}

/** Native share_seen: remember an exposure even when already seen, so another
 * client's mark-unread isn't undone until this pane leaves/re-enters view.
 * No local status override: the core's labels event updates every client. */
export function agentSeenReporter(write: (address: string, turn: number) => Promise<unknown>) {
  const looked = new Map<string, number>();
  const pending = new Map<string, symbol>();
  return (terminals: TerminalInfo[], visible: readonly string[], foreground: boolean): void => {
    const looking = new Set(foreground ? visible : []);
    for (const address of looked.keys()) if (!looking.has(address)) looked.delete(address);
    for (const terminal of terminals) {
      const address = terminalAddress(terminal), turn = agentTurn(terminal);
      if (!looking.has(address) || terminal.state !== 'live' || reportedStatus(terminal) === 'unreported' || turn === undefined || looked.get(address) === turn) continue;
      looked.set(address, turn);
      const seen = seenTurn(terminal);
      if (seen !== undefined && seen >= BigInt(turn)) continue;
      const attempt = Symbol();
      pending.set(address, attempt);
      void write(address, turn).catch(() => {
        if (pending.get(address) === attempt && looked.get(address) === turn) looked.delete(address);
      }).finally(() => { if (pending.get(address) === attempt) pending.delete(address); });
    }
  };
}
