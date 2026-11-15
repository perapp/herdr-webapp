/** A delayed IME/textarea flush is text input, not permission to submit a
 * prompt. Only an explicit Enter/line-break action or exact paste can send a
 * line ending. Parser replies bypass this check and retain their native tag.
 */
export class TerminalSubmission {
  private enter = false;
  private paste: string | undefined;
  markKey(event: { isTrusted: boolean; key: string; isComposing?: boolean; keyCode?: number; ctrlKey?: boolean }): void {
    if (!event.isTrusted) return;
    const lineEnding = event.key === 'Enter' || (event.ctrlKey && ['j', 'm'].includes(event.key.toLowerCase()));
    this.enter = !!lineEnding && !event.isComposing && event.keyCode !== 229;
    this.paste = undefined;
  }
  markInput(event: { isTrusted: boolean; inputType: string }): void {
    if (!event.isTrusted) return;
    if (event.inputType === 'insertLineBreak' || event.inputType === 'insertParagraph') this.enter = true;
    else if (event.inputType === 'insertText' || event.inputType === 'insertCompositionText') this.enter = false;
  }
  markEnter(event: { isTrusted: boolean }): void { if (event.isTrusted) this.enter = true; }
  markShortcut(event: { isTrusted: boolean }, data: string): void {
    if (!event.isTrusted) return;
    this.enter = data === '\r' || data === '\n';
    this.paste = undefined;
  }
  markPaste(event: { isTrusted: boolean; text: string }): void {
    if (event.isTrusted) this.paste = event.text.replace(/\r?\n/g, '\r');
  }
  allow(data: string): boolean {
    if (!/[\r\n]/u.test(data)) return true;
    const paste = this.paste;
    this.paste = undefined;
    if (paste !== undefined && (data === paste || data === `\x1b[200~${paste}\x1b[201~`)) return true;
    if (this.enter && (data === '\r' || data === '\n' || data === '\r\n' || data === '\x1b\r')) {
      this.enter = false; return true;
    }
    // Drop the whole unexpected textarea flush, not merely its line ending:
    // partial forwarding could duplicate the draft already held by the editor.
    return false;
  }
  clear(): void { this.enter = false; this.paste = undefined; }
}
