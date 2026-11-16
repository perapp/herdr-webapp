/** QR modules as one SVG path, drawn locally: the link never leaves the page. */
export interface QrImage { size: number; path: string }
export async function qrImage(text: string): Promise<QrImage> {
  const { default: qrcode } = await import('qrcode-generator');
  // Low error correction keeps a long private link's modules large enough to scan.
  const code = qrcode(0, 'L'); code.addData(text, 'Byte'); code.make();
  const count = code.getModuleCount(), quiet = 4;
  let path = '';
  for (let row = 0; row < count; row++) {
    for (let column = 0; column < count; column++) {
      if (code.isDark(row, column)) path += `M${column + quiet} ${row + quiet}h1v1h-1z`;
    }
  }
  return { size: count + quiet * 2, path };
}
