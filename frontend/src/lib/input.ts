/** Parser replies default to automatic; only trusted user actions grant input.
 * DOM capture listeners can be separated from xterm's handlers by a microtask,
 * and IME completion is deferred. Remember their exact payload, not a timer-wide
 * input permission that could promote an unrelated parser-generated reply.
 */
export class InputGate {
  private epoch = 0;
  private user = false;
  private pending: string[] = [];
  mark(event: { isTrusted: boolean }): void {
    if (!event.isTrusted) return;
    this.user = true;
    const epoch = ++this.epoch;
    queueMicrotask(() => { if (this.epoch === epoch) this.user = false; });
  }
  private expect(data: string): void {
    if (!data) return;
    this.pending.push(data);
    if (this.pending.length > 8) this.pending.shift();
  }
  markComposition(event: { isTrusted: boolean; data: string }): void {
    if (!event.isTrusted || !event.data || !printable(event.data)) return;
    this.mark(event); this.expect(event.data);
  }
  markInput(event: { isTrusted: boolean; data: string | null; inputType: string }): void {
    if (!event.isTrusted) return;
    this.mark(event);
    if (event.inputType === 'deleteContentBackward') this.expect('\x7f');
    else if (event.inputType === 'insertLineBreak' || event.inputType === 'insertParagraph') this.expect('\r');
    else if (event.data && printable(event.data)) this.expect(event.data);
  }
  markPaste(event: { isTrusted: boolean; text: string }): void {
    if (!event.isTrusted) return;
    this.mark(event); this.expect(event.text.replace(/\r?\n/g, '\r'));
  }
  classify(data?: string): 'input' | 'automatic' {
    if (data) {
      const index = this.pending.findIndex(text => data === text || data === `\x1b[200~${text}\x1b[201~` || (printable(data) && data.startsWith(text)));
      if (index >= 0) { this.pending.splice(0, index + 1); return 'input'; }
    }
    return this.user ? 'input' : 'automatic';
  }
  clear(): void { this.epoch++; this.user = false; this.pending = []; }
}
function printable(data: string): boolean { return [...data].every(c => c >= ' ' && c !== '\x7f'); }
// 4096 UTF-16 units: <=12KiB UTF-8 and <=24KiB even when JSON-escaped.
// Avoid splitting a surrogate pair (emoji) between WebSocket messages.
export function* inputChunks(data: string): Generator<string> {
  for (let start = 0; start < data.length;) {
    let end = Math.min(start + 4096, data.length);
    const last = data.charCodeAt(end - 1);
    if (end < data.length && last >= 0xd800 && last <= 0xdbff) end--;
    yield data.slice(start, end);
    start = end;
  }
}
export function binaryInput(data: string): Uint8Array { return Uint8Array.from(data, c => c.charCodeAt(0) & 255); }
export function retryDelay(attempt: number): number { return Math.min(1000 * 2 ** Math.min(attempt, 5), 30000); }
export function validSize(cols: unknown, rows: unknown): cols is number {
  return Number.isInteger(cols) && Number.isInteger(rows) && (cols as number) >= 2 && (rows as number) >= 1 && (cols as number) <= 1000 && (rows as number) <= 1000 && (cols as number) * (rows as number) <= 250000;
}
