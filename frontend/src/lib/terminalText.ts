interface Line { isWrapped: boolean; translateToString(trimRight?: boolean): string }
export interface TextBuffer { viewportY: number; getLine(y: number): Line | undefined }

/** The rows currently on screen as plain text. Soft-wrapped rows rejoin into
 * their logical line; trailing blank rows are dropped.
 */
export function screenText(buffer: TextBuffer, rows: number): string {
  const lines: string[] = [];
  const end = buffer.viewportY + rows;
  for (let y = buffer.viewportY; y < end; y++) {
    const line = buffer.getLine(y);
    if (!line) break;
    // A row continued below keeps its full width: its trailing spaces are text.
    const continued = y + 1 < end && buffer.getLine(y + 1)?.isWrapped === true;
    const text = line.translateToString(!continued);
    if (line.isWrapped && lines.length) lines[lines.length - 1] += text; else lines.push(text);
  }
  while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
  return lines.join('\n');
}
